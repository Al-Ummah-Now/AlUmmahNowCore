//! Text shaping for titles and burned-in captions (Epic 30.6): bidi reordering, line breaking and
//! HarfBuzz-compatible shaping, so Arabic joins correctly and mixed text reads in the right order.

use std::ops::Range;

use aun_common::{Error, Result};
use harfrust::{Buffer, Direction, Font, Language, Script, ShapeOptions as HbOptions, ShaperFont};
use serde::{Deserialize, Serialize};
use unicode_bidi::{BidiInfo, Level, ParagraphInfo};
use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_script::{Script as UScript, UnicodeScript};

/// Longest accepted text, in bytes. Line fitting re-shapes candidates, so input must be bounded.
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Paragraph direction. `Auto` follows the first strong character (Unicode rule P2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextDirection {
    #[default]
    Auto,
    Ltr,
    Rtl,
}

/// Options for [`shape_text`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ShapeOptions {
    pub direction: TextDirection,
    /// Wraps lines to this width, in the same unit as `size`.
    pub max_width: Option<f64>,
    /// BCP 47 language, for language-specific glyph forms (for example `ar`, `ur`).
    pub language: Option<String>,
}

/// One glyph. Advances and offsets are in the same unit as `size`; `y` grows upwards.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionedGlyph {
    pub glyph_id: u32,
    /// Byte offset into the original text of the character this glyph came from.
    pub cluster: u32,
    pub x_advance: f64,
    pub y_advance: f64,
    pub x_offset: f64,
    pub y_offset: f64,
}

/// A run of one direction and script. Glyphs are in visual order, left to right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlyphRun {
    pub direction: TextDirection,
    pub bidi_level: u8,
    /// ISO 15924 script tag, for example `Arab` or `Latn`.
    pub script: String,
    /// Byte range of the run in the original text.
    pub logical_start: u32,
    pub logical_end: u32,
    pub text: String,
    pub width: f64,
    pub glyphs: Vec<PositionedGlyph>,
}

/// A line. `runs` are in visual order, left to right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapedLine {
    pub logical_start: u32,
    pub logical_end: u32,
    pub width: f64,
    pub runs: Vec<GlyphRun>,
}

/// The result of [`shape_text`]: lines in reading order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapedText {
    /// The resolved direction of the first paragraph (`Ltr` or `Rtl`).
    pub base_direction: TextDirection,
    pub width: f64,
    pub lines: Vec<ShapedLine>,
}

/// Shapes `text` with one font at `size` (any unit; output uses the same one).
pub fn shape_text(text: &str, font_data: &[u8], size: f64, options: &ShapeOptions) -> Result<ShapedText> {
    if !(size > 0.0 && size.is_finite()) {
        return Err(Error::InvalidInput("font size must be positive".to_owned()));
    }
    if let Some(width) = options.max_width
        && !(width > 0.0 && width.is_finite())
    {
        return Err(Error::InvalidInput("max width must be positive".to_owned()));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::InvalidInput(format!(
            "text is longer than {MAX_TEXT_BYTES} bytes"
        )));
    }
    let font =
        Font::new(font_data.to_vec(), 0).ok_or_else(|| Error::InvalidInput("cannot read font".to_owned()))?;
    let upem = f64::from(font.units_per_em());
    if upem <= 0.0 {
        return Err(Error::InvalidInput("font has no units per em".to_owned()));
    }
    let shaper = Shaper {
        text,
        font: ShaperFont::new(&font),
        scale: size / upem,
        language: match &options.language {
            Some(tag) => Some(
                tag.parse::<Language>()
                    .map_err(|_| Error::InvalidInput(format!("bad language tag {tag}")))?,
            ),
            None => None,
        },
    };

    let base = match options.direction {
        TextDirection::Auto => None,
        TextDirection::Ltr => Some(Level::ltr()),
        TextDirection::Rtl => Some(Level::rtl()),
    };
    let bidi = BidiInfo::new(text, base);
    let breaks: Vec<usize> = linebreaks(text)
        .filter(|(_, kind)| *kind == BreakOpportunity::Allowed)
        .map(|(at, _)| at)
        .collect();

    let mut lines = Vec::new();
    for paragraph in &bidi.paragraphs {
        let end = trim_end(text, paragraph.range.clone());
        let mut start = paragraph.range.start;
        if start == end {
            lines.push(shaper.line(&bidi, paragraph, start..start)?);
            continue;
        }
        if let Some(max_width) = options.max_width {
            let candidates: Vec<usize> = breaks
                .iter()
                .copied()
                .filter(|b| *b > start && *b < end)
                .collect();
            let mut settled: Option<usize> = None;
            for candidate in candidates {
                let probe = start..trim_end(text, start..candidate);
                if shaper.line(&bidi, paragraph, probe)?.width <= max_width {
                    settled = Some(candidate);
                    continue;
                }
                if let Some(cut) = settled.take() {
                    lines.push(shaper.line(&bidi, paragraph, start..trim_end(text, start..cut))?);
                    start = cut;
                }
                // The word that overflowed either starts the new line or, if it is too long
                // for any line, occupies one by itself.
                let word = start..trim_end(text, start..candidate);
                let shaped = shaper.line(&bidi, paragraph, word)?;
                if shaped.width <= max_width {
                    settled = Some(candidate);
                } else {
                    lines.push(shaped);
                    start = candidate;
                }
            }
            let rest = shaper.line(&bidi, paragraph, start..end)?;
            if rest.width > max_width
                && let Some(cut) = settled
            {
                lines.push(shaper.line(&bidi, paragraph, start..trim_end(text, start..cut))?);
                start = cut;
                lines.push(shaper.line(&bidi, paragraph, start..end)?);
            } else {
                lines.push(rest);
            }
        } else {
            lines.push(shaper.line(&bidi, paragraph, start..end)?);
        }
    }

    let base_direction = match bidi.paragraphs.first() {
        Some(p) if p.level.is_rtl() => TextDirection::Rtl,
        Some(_) => TextDirection::Ltr,
        None => match options.direction {
            TextDirection::Rtl => TextDirection::Rtl,
            _ => TextDirection::Ltr,
        },
    };
    let width = lines.iter().map(|l| l.width).fold(0.0, f64::max);
    Ok(ShapedText {
        base_direction,
        width,
        lines,
    })
}

/// The end of `range` without trailing whitespace and paragraph separators.
fn trim_end(text: &str, range: Range<usize>) -> usize {
    range.start + text[range].trim_end().len()
}

struct Shaper<'a> {
    text: &'a str,
    font: ShaperFont<'a, 'a>,
    scale: f64,
    language: Option<Language>,
}

impl Shaper<'_> {
    fn line(
        &self,
        bidi: &BidiInfo<'_>,
        paragraph: &ParagraphInfo,
        range: Range<usize>,
    ) -> Result<ShapedLine> {
        let mut runs = Vec::new();
        if !range.is_empty() {
            let (levels, level_runs) = bidi.visual_runs(paragraph, range.clone());
            for run in level_runs {
                let level = levels[run.start];
                let mut pieces = split_by_script(self.text, run);
                if level.is_rtl() {
                    pieces.reverse();
                }
                for (piece, script) in pieces {
                    runs.push(self.run(piece, script, level)?);
                }
            }
        }
        let width = runs.iter().map(|r: &GlyphRun| r.width).sum();
        Ok(ShapedLine {
            logical_start: range.start as u32,
            logical_end: range.end as u32,
            width,
            runs,
        })
    }

    fn run(&self, range: Range<usize>, script: UScript, level: Level) -> Result<GlyphRun> {
        let mut buffer = Buffer::new();
        buffer.push_str(&self.text[range.clone()]);
        buffer.set_direction(if level.is_rtl() {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        });
        buffer.set_script(script.short_name().parse::<Script>().ok());
        buffer.set_language(self.language.clone());
        buffer.guess_segment_properties();
        harfrust::shape(&self.font, &mut buffer, HbOptions::new())
            .map_err(|e| Error::Internal(format!("shaping failed: {e:?}")))?;

        let mut width = 0.0;
        let glyphs: Vec<PositionedGlyph> = buffer
            .glyph_infos()
            .iter()
            .zip(buffer.glyph_positions())
            .map(|(info, position)| {
                let x_advance = f64::from(position.x_advance) * self.scale;
                width += x_advance;
                PositionedGlyph {
                    glyph_id: info.glyph_id,
                    cluster: info.cluster + range.start as u32,
                    x_advance,
                    y_advance: f64::from(position.y_advance) * self.scale,
                    x_offset: f64::from(position.x_offset) * self.scale,
                    y_offset: f64::from(position.y_offset) * self.scale,
                }
            })
            .collect();
        Ok(GlyphRun {
            direction: if level.is_rtl() {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            },
            bidi_level: level.number(),
            script: script.short_name().to_owned(),
            logical_start: range.start as u32,
            logical_end: range.end as u32,
            text: self.text[range].to_owned(),
            width,
            glyphs,
        })
    }
}

/// Splits a level run by script. Common and inherited characters (spaces, digits, marks) join the
/// script around them, so a space does not break Arabic joining context or kerning.
fn split_by_script(text: &str, run: Range<usize>) -> Vec<(Range<usize>, UScript)> {
    let is_neutral = |s: UScript| matches!(s, UScript::Common | UScript::Inherited);
    let mut pieces: Vec<(Range<usize>, UScript)> = Vec::new();
    for (offset, ch) in text[run.clone()].char_indices() {
        let at = run.start + offset;
        let script = ch.script();
        let next = at + ch.len_utf8();
        match pieces.last_mut() {
            Some((range, current)) if is_neutral(script) || *current == script => range.end = next,
            Some((range, current)) if is_neutral(*current) => {
                // Only neutrals so far: they adopt the first real script.
                *current = script;
                range.end = next;
            }
            _ => pieces.push((at..next, script)),
        }
    }
    pieces
}
