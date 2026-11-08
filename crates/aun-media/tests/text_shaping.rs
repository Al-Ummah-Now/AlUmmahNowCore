#![cfg(feature = "harfbuzz")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use aun_media::text::{GlyphRun, ShapeOptions, ShapedText, TextDirection, shape_text};

/// Amiri Regular (SIL OFL 1.1), subset to Arabic and ASCII. See `fixtures/fonts/OFL.txt`.
const FONT: &[u8] = include_bytes!("fixtures/fonts/Amiri-Regular-subset.ttf");
const SIZE: f64 = 48.0;

fn shape(text: &str) -> ShapedText {
    shape_text(text, FONT, SIZE, &ShapeOptions::default()).unwrap()
}

fn single_run(text: &str) -> GlyphRun {
    let shaped = shape(text);
    assert_eq!(shaped.lines.len(), 1);
    assert_eq!(shaped.lines[0].runs.len(), 1, "{:#?}", shaped.lines[0].runs);
    shaped.lines[0].runs[0].clone()
}

fn glyph_for_cluster(run: &GlyphRun, cluster: u32) -> u32 {
    run.glyphs.iter().find(|g| g.cluster == cluster).unwrap().glyph_id
}

#[test]
fn bismillah_shapes_right_to_left() {
    let text = "بسم الله الرحمن الرحيم";
    let shaped = shape(text);
    assert_eq!(shaped.base_direction, TextDirection::Rtl);
    assert_eq!(shaped.lines.len(), 1);
    let run = &shaped.lines[0].runs[0];
    assert_eq!(run.direction, TextDirection::Rtl);
    assert_eq!(run.script, "Arab");
    assert_eq!(run.text, text);
    assert!(run.width > 0.0);
    assert!(run.glyphs.iter().all(|g| g.glyph_id != 0), "no .notdef");

    // Visual order is left to right, so the logically first character is drawn last.
    let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(*clusters.last().unwrap(), 0);
    assert!(clusters.windows(2).all(|w| w[0] >= w[1]), "{clusters:?}");
}

#[test]
fn arabic_letters_take_contextual_forms() {
    // U+0628 BEH alone, then initial, medial and final in a three-letter word.
    let isolated = single_run("ب");
    let word = single_run("ببب");
    assert_eq!(word.glyphs.len(), 3);
    let (initial, medial, last) = (
        glyph_for_cluster(&word, 0),
        glyph_for_cluster(&word, 2),
        glyph_for_cluster(&word, 4),
    );
    let alone = isolated.glyphs[0].glyph_id;
    assert_ne!(initial, alone);
    assert_ne!(medial, alone);
    assert_ne!(last, alone);
    assert_ne!(initial, medial);
    assert_ne!(initial, last);
    assert_ne!(medial, last);
}

#[test]
fn non_joining_letters_break_the_join() {
    // DAL does not join to the following letter, so BEH after it takes its initial form again.
    let after_dal = single_run("دب");
    let beh = glyph_for_cluster(&after_dal, 2);
    let joined = single_run("بب");
    let final_form = glyph_for_cluster(&joined, 2);
    assert_ne!(beh, final_form);
}

#[test]
fn lam_alef_takes_its_special_form() {
    // Amiri draws lam-alef as two contextual glyphs rather than one ligature glyph. Either way
    // both letters must differ from how they look in other contexts.
    let pair = single_run("لا");
    let lam_elsewhere = single_run("لب");
    let alef_alone = single_run("ا");
    assert_ne!(glyph_for_cluster(&pair, 0), glyph_for_cluster(&lam_elsewhere, 0));
    assert_ne!(glyph_for_cluster(&pair, 2), alef_alone.glyphs[0].glyph_id);
}

#[test]
fn harakat_stay_in_their_cluster() {
    // BEH + FATHA: a mark never gets its own advance and shares the base's cluster.
    let run = single_run("بَ");
    assert!(run.glyphs.len() <= 2);
    assert!(run.glyphs.iter().all(|g| g.cluster == 0));
    if let [mark_candidate, ..] = run.glyphs.as_slice() {
        let advanced = run.glyphs.iter().filter(|g| g.x_advance > 0.0).count();
        assert_eq!(advanced, 1, "{mark_candidate:?}");
    }
}

#[test]
fn latin_shapes_left_to_right() {
    let run = single_run("Hello");
    assert_eq!(run.direction, TextDirection::Ltr);
    assert_eq!(run.script, "Latn");
    assert_eq!(run.glyphs.len(), 5);
    let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(clusters, vec![0, 1, 2, 3, 4]);
    assert!(run.glyphs.iter().all(|g| g.glyph_id != 0));
}

#[test]
fn mixed_arabic_english_digits_order_visually() {
    let text = "كتاب Book 123 جديد";
    let shaped = shape(text);
    assert_eq!(shaped.base_direction, TextDirection::Rtl);
    let visual: Vec<String> = shaped.lines[0]
        .runs
        .iter()
        .map(|r| r.text.trim().to_owned())
        .collect();
    // In an RTL paragraph the last logical word is leftmost; the LTR island keeps its own order.
    assert_eq!(visual, vec!["جديد", "Book 123", "كتاب"]);
    let directions: Vec<_> = shaped.lines[0].runs.iter().map(|r| r.direction).collect();
    assert_eq!(
        directions,
        vec![TextDirection::Rtl, TextDirection::Ltr, TextDirection::Rtl]
    );
    assert!(shaped.lines[0].runs[1].bidi_level > shaped.lines[0].runs[0].bidi_level);
}

#[test]
fn digits_inside_arabic_keep_reading_order() {
    let shaped = shape("الصفحة 42");
    let runs = &shaped.lines[0].runs;
    let digits = runs.iter().find(|r| r.text == "42").unwrap();
    assert_eq!(digits.direction, TextDirection::Ltr);
    let clusters: Vec<u32> = digits.glyphs.iter().map(|g| g.cluster).collect();
    assert!(clusters[0] < clusters[1], "{clusters:?}");
    // The number sits to the left of the Arabic word in an RTL line.
    assert_eq!(runs[0].text, "42");
}

#[test]
fn explicit_direction_overrides_detection() {
    let options = ShapeOptions {
        direction: TextDirection::Rtl,
        ..Default::default()
    };
    let shaped = shape_text("abc", FONT, SIZE, &options).unwrap();
    assert_eq!(shaped.base_direction, TextDirection::Rtl);
    let shaped = shape("abc");
    assert_eq!(shaped.base_direction, TextDirection::Ltr);
}

#[test]
fn newlines_make_separate_lines() {
    let shaped = shape("بسم الله\nHello");
    assert_eq!(shaped.lines.len(), 2);
    assert_eq!(shaped.lines[0].runs[0].direction, TextDirection::Rtl);
    assert_eq!(shaped.lines[1].runs[0].direction, TextDirection::Ltr);
    assert_eq!(shaped.lines[1].runs[0].text, "Hello");
}

#[test]
fn wrapping_fits_lines_and_loses_no_text() {
    let text = "بسم الله الرحمن الرحيم الحمد لله رب العالمين الرحمن الرحيم مالك يوم الدين";
    let whole = shape(text);
    let limit = whole.width / 3.0;
    let options = ShapeOptions {
        max_width: Some(limit),
        ..Default::default()
    };
    let shaped = shape_text(text, FONT, SIZE, &options).unwrap();
    assert!(shaped.lines.len() >= 3, "{}", shaped.lines.len());
    assert!(
        shaped.lines.iter().all(|l| l.width <= limit),
        "{:?}",
        shaped.width
    );

    // Lines cover the text in reading order, separated only by whitespace.
    let mut position = 0usize;
    for line in &shaped.lines {
        let gap = &text[position..line.logical_start as usize];
        assert!(gap.trim().is_empty(), "{gap:?}");
        position = line.logical_end as usize;
    }
    assert_eq!(position, text.len());
}

#[test]
fn wrapping_reshapes_each_line() {
    // Letters at a line end must take final forms, which only re-shaping per line gives.
    let text = "ببب ببب";
    let whole = shape(text);
    let options = ShapeOptions {
        max_width: Some(whole.width * 0.7),
        ..Default::default()
    };
    let shaped = shape_text(text, FONT, SIZE, &options).unwrap();
    assert_eq!(shaped.lines.len(), 2);
    let first = &shaped.lines[0].runs[0];
    let reference = single_run("ببب");
    let expected: Vec<u32> = reference.glyphs.iter().map(|g| g.glyph_id).collect();
    let actual: Vec<u32> = first.glyphs.iter().map(|g| g.glyph_id).collect();
    assert_eq!(actual, expected);
}

#[test]
fn an_overlong_word_gets_its_own_line() {
    let options = ShapeOptions {
        max_width: Some(1.0),
        ..Default::default()
    };
    let shaped = shape_text("hello world", FONT, SIZE, &options).unwrap();
    assert_eq!(shaped.lines.len(), 2);
}

#[test]
fn size_scales_advances() {
    let a = shape_text("Hello", FONT, 24.0, &ShapeOptions::default()).unwrap();
    let b = shape_text("Hello", FONT, 48.0, &ShapeOptions::default()).unwrap();
    assert!((b.width - 2.0 * a.width).abs() < 1e-6);
}

#[test]
fn empty_text_has_no_runs() {
    let shaped = shape("");
    assert!(shaped.lines.iter().all(|l| l.runs.is_empty()));
    assert_eq!(shaped.width, 0.0);
}

#[test]
fn rejects_bad_input() {
    let opts = ShapeOptions::default();
    assert!(shape_text("a", FONT, 0.0, &opts).is_err());
    assert!(shape_text("a", FONT, f64::NAN, &opts).is_err());
    assert!(shape_text("a", b"not a font", 12.0, &opts).is_err());
    let huge = "a".repeat(70_000);
    assert!(shape_text(&huge, FONT, 12.0, &opts).is_err());
    let bad_width = ShapeOptions {
        max_width: Some(-1.0),
        ..Default::default()
    };
    assert!(shape_text("a", FONT, 12.0, &bad_width).is_err());
}
