#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

/// A synthetic progressive MP4: video at 30 fps (timescale 30 000, keyframe every `gop` frames,
/// B-frame style composition offsets) and optional AAC-like audio (48 kHz, 1024-sample frames).
/// Sample bytes encode (track, index) so copies can be checked byte for byte.
struct Synth {
    frames: u32,
    gop: u32,
    audio: bool,
    co64: bool,
}

fn sample_bytes(track: u8, i: u32) -> Vec<u8> {
    let len = 20 + (i % 7) as usize * 3 + if track == 1 && i.is_multiple_of(60) { 50 } else { 0 };
    let mut v = vec![track; len];
    v[1..5].copy_from_slice(&i.to_be_bytes());
    v
}

fn audio_frames(video_frames: u32) -> u32 {
    // 48 000 / 1024 frames per second, over the video's length
    ((u64::from(video_frames) * 48_000) / (30 * 1024)) as u32
}

impl Synth {
    fn build(&self, dir: &Path) -> std::path::PathBuf {
        let mut mdat = Vec::new();
        let base = 32u64; // ftyp(24) + mdat header(8)
        // Video in chunks of 10 samples, audio in chunks of 16, interleaved
        let vids: Vec<Vec<u8>> = (0..self.frames).map(|i| sample_bytes(1, i)).collect();
        let auds: Vec<Vec<u8>> = if self.audio {
            (0..audio_frames(self.frames))
                .map(|i| sample_bytes(2, i))
                .collect()
        } else {
            vec![]
        };
        let (mut v_chunks, mut a_chunks) = (Vec::new(), Vec::new());
        let (mut vi, mut ai) = (0usize, 0usize);
        while vi < vids.len() || ai < auds.len() {
            if vi < vids.len() {
                v_chunks.push(base + mdat.len() as u64);
                for s in vids.iter().skip(vi).take(10) {
                    mdat.extend_from_slice(s);
                }
                vi += 10;
            }
            if ai < auds.len() {
                a_chunks.push(base + mdat.len() as u64);
                for s in auds.iter().skip(ai).take(16) {
                    mdat.extend_from_slice(s);
                }
                ai += 16;
            }
        }
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isomavc1");
        let mut file = bx(b"ftyp", &ftyp);
        assert_eq!(file.len(), 24);
        file.extend(bx(b"mdat", &mdat));

        let mut moov = full(b"mvhd", 0, 0, &{
            let mut b = vec![0u8; 8];
            b.extend_from_slice(&1000u32.to_be_bytes());
            b.extend_from_slice(&(self.frames * 1000 / 30).to_be_bytes());
            b.extend(vec![0u8; 80]);
            b
        });
        let sizes_v: Vec<u32> = vids.iter().map(|s| s.len() as u32).collect();
        let sync: Vec<u32> = (0..self.frames)
            .filter(|i| i % self.gop == 0)
            .map(|i| i + 1)
            .collect();
        // Composition offsets: keyframes +2 frames, others +1 (positive, like a B-frame stream)
        let ctts: Vec<(u32, u32)> = (0..self.frames)
            .map(|i| (1, if i % self.gop == 0 { 2000 } else { 1000 }))
            .collect();
        moov.extend(self.trak(
            1,
            b"vide",
            30_000,
            1000,
            &sizes_v,
            &v_chunks,
            10,
            Some(&sync),
            Some(&ctts),
        ));
        if self.audio {
            let sizes_a: Vec<u32> = auds.iter().map(|s| s.len() as u32).collect();
            moov.extend(self.trak(2, b"soun", 48_000, 1024, &sizes_a, &a_chunks, 16, None, None));
        }
        file.extend(bx(b"moov", &moov));
        let path = dir.join("rung.mp4");
        fs::write(&path, file).unwrap();
        path
    }

    #[allow(clippy::too_many_arguments)]
    fn trak(
        &self,
        id: u32,
        handler: &[u8; 4],
        timescale: u32,
        delta: u32,
        sizes: &[u32],
        chunks: &[u64],
        per_chunk: u32,
        sync: Option<&[u32]>,
        ctts: Option<&[(u32, u32)]>,
    ) -> Vec<u8> {
        let n = sizes.len() as u32;
        let mut tkhd = vec![0u8; 8];
        tkhd.extend_from_slice(&id.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&(n * delta).to_be_bytes());
        tkhd.extend(vec![0u8; 60]);
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&(n * delta).to_be_bytes());
        mdhd.extend_from_slice(&[0x15, 0xc7, 0, 0]);
        let mut hdlr = vec![0u8; 4];
        hdlr.extend_from_slice(handler);
        hdlr.extend(vec![0u8; 12]);
        hdlr.extend_from_slice(b"h\0");
        let mut stsd_body = 1u32.to_be_bytes().to_vec();
        stsd_body.extend(bx(if handler == b"vide" { b"avc1" } else { b"mp4a" }, &[7u8; 30]));
        let mut stbl = full(b"stsd", 0, 0, &stsd_body);
        stbl.extend(full(
            b"stts",
            0,
            0,
            &[1u32.to_be_bytes(), n.to_be_bytes(), delta.to_be_bytes()].concat(),
        ));
        if let Some(ctts) = ctts {
            let mut b = (ctts.len() as u32).to_be_bytes().to_vec();
            for (c, o) in ctts {
                b.extend_from_slice(&c.to_be_bytes());
                b.extend_from_slice(&o.to_be_bytes());
            }
            stbl.extend(full(b"ctts", 0, 0, &b));
        }
        if let Some(sync) = sync {
            let mut b = (sync.len() as u32).to_be_bytes().to_vec();
            for s in sync {
                b.extend_from_slice(&s.to_be_bytes());
            }
            stbl.extend(full(b"stss", 0, 0, &b));
        }
        let last = n - per_chunk * (chunks.len() as u32 - 1);
        let mut stsc = Vec::new();
        let entries: Vec<(u32, u32)> = if last == per_chunk || chunks.len() == 1 {
            vec![(1, if chunks.len() == 1 { last } else { per_chunk })]
        } else {
            vec![(1, per_chunk), (chunks.len() as u32, last)]
        };
        stsc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (f, p) in entries {
            stsc.extend_from_slice(&f.to_be_bytes());
            stsc.extend_from_slice(&p.to_be_bytes());
            stsc.extend_from_slice(&1u32.to_be_bytes());
        }
        stbl.extend(full(b"stsc", 0, 0, &stsc));
        let mut stsz = 0u32.to_be_bytes().to_vec();
        stsz.extend_from_slice(&n.to_be_bytes());
        for s in sizes {
            stsz.extend_from_slice(&s.to_be_bytes());
        }
        stbl.extend(full(b"stsz", 0, 0, &stsz));
        let mut co = (chunks.len() as u32).to_be_bytes().to_vec();
        for c in chunks {
            if self.co64 {
                co.extend_from_slice(&c.to_be_bytes());
            } else {
                co.extend_from_slice(&(*c as u32).to_be_bytes());
            }
        }
        stbl.extend(full(if self.co64 { b"co64" } else { b"stco" }, 0, 0, &co));
        let header = if handler == b"vide" {
            full(b"vmhd", 0, 1, &[0u8; 8])
        } else {
            full(b"smhd", 0, 0, &[0u8; 4])
        };
        let mut minf = header;
        minf.extend(bx(b"stbl", &stbl));
        let mut mdia = full(b"mdhd", 0, 0, &mdhd);
        mdia.extend(full(b"hdlr", 0, 0, &hdlr));
        mdia.extend(bx(b"minf", &minf));
        let mut trak = full(b"tkhd", 0, 3, &tkhd);
        trak.extend(bx(b"mdia", &mdia));
        bx(b"trak", &trak)
    }
}

/// One `trun` entry: (duration, size, flags, composition offset).
type RunSample = (u32, u32, u32, i32);
/// One track of a segment: (track id, base decode time, samples, their bytes).
type SegmentTrack = (u32, u64, Vec<RunSample>, Vec<u8>);

/// What a segment holds, read back, and its sequence number.
fn read_segment(bytes: &[u8]) -> (Vec<SegmentTrack>, u32) {
    let top = children(bytes).unwrap();
    assert_eq!(&top[0].kind, b"styp");
    let moof_box = need(&top, b"moof").unwrap();
    let moof_start = moof_box.whole.as_ptr() as usize - bytes.as_ptr() as usize;
    let parts = children(moof_box.payload).unwrap();
    let seq = u32::from_be_bytes(need(&parts, b"mfhd").unwrap().payload[4..8].try_into().unwrap());
    let mut out = Vec::new();
    for traf in parts.iter().filter(|b| &b.kind == b"traf") {
        let t = children(traf.payload).unwrap();
        let tfhd = need(&t, b"tfhd").unwrap().payload;
        assert_eq!(
            u32::from_be_bytes([0, tfhd[1], tfhd[2], tfhd[3]]),
            0x02_0000,
            "default-base-is-moof"
        );
        let id = u32::from_be_bytes(tfhd[4..8].try_into().unwrap());
        let tfdt = need(&t, b"tfdt").unwrap().payload;
        let base = u64::from_be_bytes(tfdt[4..12].try_into().unwrap());
        let mut c = Cursor::new(need(&t, b"trun").unwrap().payload);
        c.full().unwrap();
        let n = c.u32().unwrap();
        let offset = c.u32().unwrap() as i32;
        let mut samples = Vec::new();
        let mut data = Vec::new();
        let mut at = moof_start + offset as usize;
        for _ in 0..n {
            let s = (
                c.u32().unwrap(),
                c.u32().unwrap(),
                c.u32().unwrap(),
                c.u32().unwrap() as i32,
            );
            data.extend_from_slice(&bytes[at..at + s.1 as usize]);
            at += s.1 as usize;
            samples.push(s);
        }
        out.push((id, base, samples, data));
    }
    (out, seq)
}

fn package(s: &Synth) -> (tempfile::TempDir, HlsPackage) {
    let dir = tempfile::tempdir().unwrap();
    let input = s.build(dir.path());
    let out = dir.path().join("720p");
    let pkg = package_mp4(&input, &out, 6, &CancellationToken::new()).unwrap();
    (dir, pkg)
}

#[test]
fn cuts_at_the_first_keyframe_past_each_six_seconds() {
    // 20 s at 30 fps, keyframe every 2 s → segments of 6, 6, 6, 2 s
    let (dir, pkg) = package(&Synth {
        frames: 600,
        gop: 60,
        audio: true,
        co64: false,
    });
    assert_eq!(pkg.segment_seconds, vec![6.0, 6.0, 6.0, 2.0]);
    assert_eq!(pkg.duration_ms, 20_000);
    let names: Vec<&str> = pkg.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        names,
        [
            "index.m3u8",
            "init.mp4",
            "seg_00001.m4s",
            "seg_00002.m4s",
            "seg_00003.m4s",
            "seg_00004.m4s"
        ]
    );
    let out = dir.path().join("720p");
    for f in &pkg.files {
        assert_eq!(
            fs::metadata(out.join(&f.path)).unwrap().len(),
            f.bytes,
            "{}",
            f.path
        );
    }
    let playlist = fs::read_to_string(out.join("index.m3u8")).unwrap();
    assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n"));
    assert!(playlist.contains("#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.000000,\nseg_00001.m4s\n"));
    assert!(playlist.ends_with("#EXTINF:2.000000,\nseg_00004.m4s\n#EXT-X-ENDLIST\n"));
}

#[test]
fn a_longer_gop_makes_longer_segments_on_keyframes_only() {
    // Keyframe every 4 s: cuts at 8 and 12 s... first keyframe at or past 6 s is 8 s
    let (_d, pkg) = package(&Synth {
        frames: 600,
        gop: 120,
        audio: false,
        co64: false,
    });
    assert_eq!(pkg.segment_seconds, vec![8.0, 4.0, 8.0]);
}

#[test]
fn init_segment_has_codec_boxes_empty_tables_and_trex_per_track() {
    let (dir, _) = package(&Synth {
        frames: 300,
        gop: 60,
        audio: true,
        co64: false,
    });
    let init = fs::read(dir.path().join("720p/init.mp4")).unwrap();
    let top = children(&init).unwrap();
    assert_eq!(&top[0].kind, b"ftyp");
    let moov = children(need(&top, b"moov").unwrap().payload).unwrap();
    let mvex = children(need(&moov, b"mvex").unwrap().payload).unwrap();
    assert_eq!(mvex.iter().filter(|b| &b.kind == b"trex").count(), 2);
    let traks: Vec<_> = moov.iter().filter(|b| &b.kind == b"trak").collect();
    assert_eq!(traks.len(), 2);
    let trak0 = children(traks[0].payload).unwrap();
    let mdia = children(need(&trak0, b"mdia").unwrap().payload).unwrap();
    let minf = children(need(&mdia, b"minf").unwrap().payload).unwrap();
    let stbl = children(need(&minf, b"stbl").unwrap().payload).unwrap();
    let stsd = need(&stbl, b"stsd").unwrap();
    assert!(
        stsd.payload.windows(4).any(|w| w == b"avc1"),
        "codec description copied"
    );
    assert_eq!(
        need(&stbl, b"stsz").unwrap().payload,
        &[0u8; 12],
        "no samples in the init"
    );
    // Durations are zeroed: timing lives in the fragments
    let mdhd = need(&mdia, b"mdhd").unwrap().payload;
    assert_eq!(&mdhd[16..20], &[0, 0, 0, 0]);
    assert_eq!(&mdhd[12..16], &30_000u32.to_be_bytes());
}

#[test]
fn every_sample_lands_once_in_order_with_its_bytes_and_timing() {
    let synth = Synth {
        frames: 600,
        gop: 60,
        audio: true,
        co64: true,
    };
    let (dir, pkg) = package(&synth);
    let out = dir.path().join("720p");
    let (mut next_v, mut next_a) = (0u32, 0u32);
    let (mut v_dts, mut a_dts) = (0u64, 0u64);
    for (k, f) in pkg.files.iter().filter(|f| f.path.ends_with(".m4s")).enumerate() {
        let (tracks, seq) = read_segment(&fs::read(out.join(&f.path)).unwrap());
        assert_eq!(seq, k as u32 + 1);
        for (id, base, samples, data) in tracks {
            let mut expected = Vec::new();
            if id == 1 {
                assert_eq!(base, v_dts, "video tfdt continues");
                assert_eq!(samples[0].2, SYNC_FLAGS, "every segment starts on a keyframe");
                for (j, s) in samples.iter().enumerate() {
                    let i = next_v + j as u32;
                    expected.extend(sample_bytes(1, i));
                    assert_eq!(s.0, 1000);
                    assert_eq!(s.3, if i.is_multiple_of(60) { 2000 } else { 1000 });
                    assert_eq!(s.2 == SYNC_FLAGS, i.is_multiple_of(60));
                }
                next_v += samples.len() as u32;
                v_dts += samples.len() as u64 * 1000;
            } else {
                assert_eq!(base, a_dts, "audio tfdt continues");
                for (j, s) in samples.iter().enumerate() {
                    expected.extend(sample_bytes(2, next_a + j as u32));
                    assert_eq!(s.0, 1024);
                }
                // Audio stays within its segment's time (to the frame)
                let seg_end = f64::from(next_v) / 30.0;
                let audio_end = (a_dts + samples.len() as u64 * 1024) as f64 / 48_000.0;
                assert!(
                    audio_end <= seg_end + 1024.0 / 48_000.0 + 1e-9,
                    "segment {k}: audio to {audio_end} past {seg_end}"
                );
                next_a += samples.len() as u32;
                a_dts += samples.len() as u64 * 1024;
            }
            assert_eq!(data, expected, "segment {k} track {id} bytes");
        }
    }
    assert_eq!(next_v, 600);
    assert_eq!(next_a, audio_frames(600));
}

#[test]
fn refuses_fragmented_or_broken_input() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let frag = dir.path().join("frag.mp4");
    let mut f = bx(b"ftyp", b"iso6\0\0\0\0iso6");
    f.extend(bx(b"moov", &bx(b"mvex", &[])));
    f.extend(bx(b"moof", &[]));
    fs::write(&frag, f).unwrap();
    assert!(matches!(
        package_mp4(&frag, &dir.path().join("o"), 6, &cancel),
        Err(Error::Unsupported(_))
    ));

    let junk = dir.path().join("junk.mp4");
    fs::write(&junk, [0u8, 0, 0, 200, b'm', b'o', b'o', b'v', 1, 2, 3]).unwrap();
    assert!(matches!(
        package_mp4(&junk, &dir.path().join("o"), 6, &cancel),
        Err(Error::InvalidInput { .. })
    ));

    // A sample table pointing past the end of the file
    let synth = Synth {
        frames: 120,
        gop: 60,
        audio: false,
        co64: false,
    };
    let good = synth.build(dir.path());
    let mut bytes = fs::read(&good).unwrap();
    let at = bytes.windows(4).position(|w| w == b"stco").unwrap() + 12;
    bytes[at..at + 4].copy_from_slice(&0x7fff_0000u32.to_be_bytes());
    let bad = dir.path().join("bad.mp4");
    fs::write(&bad, bytes).unwrap();
    assert!(package_mp4(&bad, &dir.path().join("o2"), 6, &cancel).is_err());
}

#[test]
fn cancel_stops_between_segments() {
    let dir = tempfile::tempdir().unwrap();
    let input = Synth {
        frames: 300,
        gop: 60,
        audio: false,
        co64: false,
    }
    .build(dir.path());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        package_mp4(&input, &dir.path().join("o"), 6, &cancel),
        Err(Error::Cancelled)
    ));
}

#[test]
fn lists_rendition_files_playlist_first_per_rung() {
    let dir = tempfile::tempdir().unwrap();
    for rung in ["360p", "720p"] {
        let input = Synth {
            frames: 300,
            gop: 60,
            audio: true,
            co64: false,
        }
        .build(dir.path());
        package_mp4(&input, &dir.path().join(rung), 6, &CancellationToken::new()).unwrap();
    }
    fs::write(dir.path().join("720p/.DS_Store"), b"x").unwrap();
    let files = renditions_files(dir.path(), &["360p".into(), "720p".into()]).unwrap();
    assert_eq!(files[0].path, "360p/index.m3u8");
    assert_eq!(files.iter().filter(|f| f.path.ends_with("index.m3u8")).count(), 2);
    assert!(files.iter().all(|f| !f.path.contains(".DS_Store")));
    // 10 s → two segments, plus the init and the playlist
    assert_eq!(files.iter().filter(|f| f.path.starts_with("720p/")).count(), 4);
    assert!(renditions_files(dir.path(), &["1080p".into()]).is_err());
    assert!(renditions_files(dir.path(), &["../x".into()]).is_err());
}
