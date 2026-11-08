//! A clip of an audio track for `--detect`, made in this program (the native-detect feature): the frames of
//! the track in the clip's time are read from an MKV (see `ebml_subs::read_blocks`), decoded with
//! `audio_decode`, and written as a 16-bit mono WAV at 16 kHz, which whisper.cpp reads.

use std::io::{self, Write};
use std::path::Path;

use crate::audio_decode::{self, Codec, RATE};
use crate::ebml;
use crate::ebml_subs;

/// The frames read before and after the clip, so that the decoder has started (and is not cut short) by the
/// clip's first sample.
const PAD_MS: i64 = 2_000;

/// Write `seconds` of track `track_number` of the MKV `source`, from `start_seconds`, to `out`.
pub fn write_clip(source: &Path, track_number: u64, start_seconds: f64, seconds: f64, out: &Path) -> io::Result<()> {
    let entry = ebml::read_track_entry(source, track_number)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no such track"))?;
    let codec = match entry.codec_id.as_str() {
        "A_AAC" => {
            // the AudioSpecificConfig starts with the object type (5 bits): 29 is HE-AAC v2 (parametric stereo),
            // which is not checked here, so it is not decoded (the library has none)
            let object_type = entry.codec_private.as_ref().and_then(|c| c.first()).map(|b| b >> 3);
            if object_type == Some(29) {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "HE-AAC v2 (parametric stereo) has no decoder without ffmpeg yet"));
            }
            Codec::Aac
        }
        "A_AC3" => Codec::Ac3,
        "A_EAC3" => Codec::Eac3,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("the codec {other} has no decoder without ffmpeg (only AAC, AC-3 and E-AC-3)"),
            ))
        }
    };
    let from_ms = (start_seconds * 1000.0) as i64;
    let to_ms = from_ms + (seconds * 1000.0) as i64;
    let blocks = ebml_subs::read_blocks(source, track_number, from_ms - PAD_MS, to_ms + PAD_MS)?;
    let first = blocks.first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no sound in the clip"))?;
    let decoded_from = first.start_ms;
    let packets: Vec<Vec<u8>> = blocks.into_iter().map(|block| block.payload).collect();
    let samples = audio_decode::decode(codec, entry.sample_rate, entry.channels as u32, &packets)?;
    // the decoded samples begin with the first frame read: keep those from the clip's start, for its length
    let skip = (((from_ms - decoded_from) as f64 / 1000.0) * RATE).max(0.0) as usize;
    let take = (seconds * RATE) as usize;
    let begin = skip.min(samples.len());
    let end = (skip + take).min(samples.len());
    write_wav(out, &samples[begin..end])
}

/// A 16-bit mono WAV file at 16 kHz with the samples (floating point, from -1 to 1).
pub fn write_wav(out: &Path, samples: &[f32]) -> io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&1u16.to_le_bytes())?; // mono
    file.write_all(&(RATE as u32).to_le_bytes())?;
    file.write_all(&((RATE as u32) * 2).to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        file.write_all(&value.to_le_bytes())?;
    }
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 16-bit mono samples of an ffmpeg clip of the same window (the reference for the native one).
    fn ffmpeg_clip(source: &Path, audio_index: usize, start: f64, seconds: f64) -> Vec<f32> {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{start}"), "-t", &format!("{seconds}"), "-i"])
            .arg(source)
            .args(["-map", &format!("0:a:{audio_index}"), "-vn", "-sn", "-ac", "1", "-ar", "16000", "-f", "s16le", "-"])
            .output()
            .expect("ffmpeg runs");
        out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32767.0).collect()
    }

    /// Each real file named in MAU_CLIP_CHECK (separated by `:`): the first audio track's clip at 60 seconds,
    /// made here and by ffmpeg, must have the same length (within a tenth of a second) and be the same sound
    /// (correlation above 0.9 at the best lag of up to 0.1 seconds).
    #[test]
    fn the_native_clip_is_the_same_sound_as_ffmpeg_makes_on_the_files_named_in_mau_clip_check() {
        let Ok(list) = std::env::var("MAU_CLIP_CHECK") else { return };
        let dir = std::env::temp_dir().join(format!("mau-clip-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut failures = Vec::new();
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let tracks = ebml::read_file_tracks(source).unwrap();
            let audio = tracks.iter().find(|t| t.kind == crate::track::Kind::Audio).unwrap();
            let out = dir.join("native.wav");
            if let Err(error) = write_clip(source, audio.id as u64 + 1, 60.0, 10.0, &out) {
                failures.push(format!("{file}: {error}"));
                continue;
            }
            let bytes = std::fs::read(&out).unwrap();
            let native: Vec<f32> = bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32767.0).collect();
            let reference = ffmpeg_clip(source, 0, 60.0, 10.0);
            let length_ok = (native.len() as f64 - reference.len() as f64).abs() < 1600.0;
            let lag_range = 1600i64;
            let mut best = f64::MIN;
            for lag in -lag_range..=lag_range {
                let (a, b): (Vec<f32>, Vec<f32>) = if lag >= 0 {
                    let n = native.len().min(reference.len().saturating_sub(lag as usize));
                    (native[..n].to_vec(), reference[lag as usize..lag as usize + n].to_vec())
                } else {
                    let n = reference.len().min(native.len().saturating_sub((-lag) as usize));
                    (native[(-lag) as usize..(-lag) as usize + n].to_vec(), reference[..n].to_vec())
                };
                if a.len() < 16_000 { continue; }
                let dot: f64 = a.iter().zip(&b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
                let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
                let nb: f64 = b.iter().map(|y| (*y as f64).powi(2)).sum::<f64>().sqrt();
                if na > 0.0 && nb > 0.0 {
                    best = best.max(dot / (na * nb));
                }
            }
            eprintln!("{file}: native {} samples, ffmpeg {} samples, best correlation {best:.3}", native.len(), reference.len());
            if !length_ok || best < 0.9 {
                failures.push(format!("{file}: length {} vs {}, correlation {best:.3}", native.len(), reference.len()));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_wav_file_has_the_header_whisper_reads() {
        let dir = std::env::temp_dir().join(format!("mau-clip-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("clip.wav");
        write_wav(&out, &[0.0, 1.0, -1.0, 0.5]).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1, "mono");
        assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 16_000, "16 kHz");
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16, "16 bits");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]), 8, "four samples of two bytes");
        assert_eq!(bytes.len(), 44 + 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
