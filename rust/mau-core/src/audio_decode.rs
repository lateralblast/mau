//! Decoding compressed audio to 16 kHz mono floating point samples, with Apple's AudioToolbox (macOS only).
//!
//! `--detect` needs a short clip of each audio track, as whisper.cpp reads it (16 kHz mono). ffmpeg makes it
//! for the other platforms and for the other containers; on macOS this decodes the frames of an MKV's track
//! with the system's own converter instead, so the App Store build does not need ffmpeg for that.

use std::io;

/// The sample rate whisper.cpp reads.
pub const RATE: f64 = 16_000.0;

/// A compressed codec the converter is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Aac,
    Ac3,
    Eac3,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Codec, RATE};
    use std::ffi::c_void;
    use std::io;

    type OSStatus = i32;
    type AudioConverterRef = *mut c_void;

    #[repr(C)]
    struct Format {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PacketDescription {
        start_offset: i64,
        variable_frames: u32,
        data_size: u32,
    }

    #[repr(C)]
    struct Buffer {
        channels: u32,
        data_size: u32,
        data: *mut c_void,
    }

    #[repr(C)]
    struct BufferList {
        buffers: u32,
        buffer: Buffer,
    }

    type InputProc = extern "C" fn(AudioConverterRef, *mut u32, *mut BufferList, *mut *mut PacketDescription, *mut c_void) -> OSStatus;

    #[link(name = "AudioToolbox", kind = "framework")]
    extern "C" {
        fn AudioConverterNew(source: *const Format, destination: *const Format, converter: *mut AudioConverterRef) -> OSStatus;
        fn AudioConverterDispose(converter: AudioConverterRef) -> OSStatus;
        fn AudioConverterSetProperty(converter: AudioConverterRef, property: u32, size: u32, data: *const c_void) -> OSStatus;
        fn AudioConverterFillComplexBuffer(
            converter: AudioConverterRef,
            input: InputProc,
            user_data: *mut c_void,
            output_packets: *mut u32,
            output: *mut BufferList,
            descriptions: *mut PacketDescription,
        ) -> OSStatus;
    }

    const FORMAT_AAC: u32 = 0x6161_6320; // 'aac '
    const FORMAT_AC3: u32 = 0x6163_2D33; // 'ac-3'
    const FORMAT_EAC3: u32 = 0x6563_2D33; // 'ec-3'
    const FORMAT_LPCM: u32 = 0x6C70_636D; // 'lpcm'
    const FLAG_FLOAT_PACKED: u32 = 1 | 8;
    const MAGIC_COOKIE: u32 = 0x646D_6763; // 'dmgc', the decompression cookie

    /// The AudioSpecificConfig `asc` in an `esds` box (ISO/IEC 14496-1 descriptors), as an MP4 holds it.
    pub(super) fn esds_box(asc: &[u8]) -> Vec<u8> {
        let mut dsi = vec![0x05, asc.len() as u8];
        dsi.extend_from_slice(asc);
        // objectType 0x40 (AAC), streamType 0x15 (audio), buffer size (3), max and average bitrate (4 each)
        let mut dcd = vec![0x04, (13 + dsi.len()) as u8, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        dcd.extend_from_slice(&dsi);
        let mut es = vec![0x03, (3 + dcd.len() + 3) as u8, 0, 0, 0];
        es.extend_from_slice(&dcd);
        es.extend_from_slice(&[0x06, 0x01, 0x02]);
        let mut out = vec![0, 0, 0, (12 + es.len()) as u8];
        out.extend_from_slice(b"esds");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&es);
        out
    }

    struct Input {
        packets: Vec<Vec<u8>>,
        descriptions: Vec<PacketDescription>,
        next: usize,
    }

    extern "C" fn supply(_: AudioConverterRef, packets: *mut u32, list: *mut BufferList, descriptions: *mut *mut PacketDescription, user: *mut c_void) -> OSStatus {
        // SAFETY: `user` is the `Input` that `decode` passed in, alive for the whole call, and the converter
        // calls this on the same thread.
        let input = unsafe { &mut *(user as *mut Input) };
        if input.next >= input.packets.len() {
            // no more packets: the converter finishes what it has
            unsafe { *packets = 0 };
            return 0;
        }
        let packet = &input.packets[input.next];
        // SAFETY: the pointers are the converter's own output slots, and the packet and its description live in
        // `input`, which outlives the call.
        unsafe {
            (*list).buffers = 1;
            (*list).buffer = Buffer { channels: 1, data_size: packet.len() as u32, data: packet.as_ptr() as *mut c_void };
            *descriptions = &mut input.descriptions[input.next];
            *packets = 1;
        }
        input.next += 1;
        0
    }

    fn error(what: &str, status: OSStatus) -> io::Error {
        io::Error::new(io::ErrorKind::Other, format!("{what} (AudioToolbox error {status})"))
    }

    /// Decode the packets to 16 kHz mono samples.
    /// `config` is the codec's setup data (the AudioSpecificConfig, for AAC). The converter needs it to decode
    /// HE-AAC: without it AudioToolbox refuses the frames. The converter takes it inside an `esds` box, as an MP4
    /// has it, and the AAC-LC files checked decode the same with or without the box.
    pub fn decode(codec: Codec, sample_rate: f64, channels: u32, packets: &[Vec<u8>], config: Option<&[u8]>) -> io::Result<Vec<f32>> {
        let (format_id, frames_per_packet) = match codec {
            Codec::Aac => (FORMAT_AAC, 1024),
            Codec::Ac3 => (FORMAT_AC3, 1536),
            Codec::Eac3 => (FORMAT_EAC3, 0),
        };
        let source = Format {
            sample_rate,
            format_id,
            format_flags: 0,
            bytes_per_packet: 0,
            frames_per_packet,
            bytes_per_frame: 0,
            channels_per_frame: channels,
            bits_per_channel: 0,
            reserved: 0,
        };
        let destination = Format {
            sample_rate: RATE,
            format_id: FORMAT_LPCM,
            format_flags: FLAG_FLOAT_PACKED,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: 1,
            bits_per_channel: 32,
            reserved: 0,
        };
        let mut converter: AudioConverterRef = std::ptr::null_mut();
        // SAFETY: both formats are complete and the converter pointer is written by the call.
        let status = unsafe { AudioConverterNew(&source, &destination, &mut converter) };
        if status != 0 {
            return Err(error("the audio converter could not be made", status));
        }
        // only AAC has setup data here: the AC-3 and E-AC-3 frames carry what the converter needs
        if let (Codec::Aac, Some(config)) = (codec, config) {
            let cookie = esds_box(config);
            // SAFETY: the converter is alive, and the cookie is read for the length given.
            let status = unsafe { AudioConverterSetProperty(converter, MAGIC_COOKIE, cookie.len() as u32, cookie.as_ptr() as *const c_void) };
            if status != 0 {
                unsafe { AudioConverterDispose(converter) };
                return Err(error("the decoder setup could not be given", status));
            }
        }
        // no magic cookie: AudioToolbox refused the AAC setup data, and decodes these frames without it (the
        // AAC-LC with SBR tracks of the library were checked against ffmpeg that way)
        let mut input = Input {
            packets: packets.to_vec(),
            descriptions: packets
                .iter()
                .map(|p| PacketDescription { start_offset: 0, variable_frames: 0, data_size: p.len() as u32 })
                .collect(),
            next: 0,
        };
        let mut samples = Vec::new();
        let mut chunk = vec![0f32; 4096];
        let result = loop {
            let mut frames = chunk.len() as u32;
            let mut output = BufferList {
                buffers: 1,
                buffer: Buffer { channels: 1, data_size: (chunk.len() * 4) as u32, data: chunk.as_mut_ptr() as *mut c_void },
            };
            // SAFETY: the output buffer and the input state are alive for the call, and `supply` is the input proc.
            let status = unsafe {
                AudioConverterFillComplexBuffer(converter, supply, &mut input as *mut Input as *mut c_void, &mut frames, &mut output, std::ptr::null_mut())
            };
            if status != 0 && input.next < input.packets.len() {
                break Err(error("the frames could not be decoded", status));
            }
            if frames == 0 {
                break Ok(());
            }
            samples.extend_from_slice(&chunk[..frames as usize]);
        };
        // SAFETY: the converter was made above and is not used after this.
        unsafe { AudioConverterDispose(converter) };
        result.map(|()| samples)
    }
}

/// Decode compressed audio packets to 16 kHz mono samples (see `Codec`).
pub fn decode(codec: Codec, sample_rate: f64, channels: u32, packets: &[Vec<u8>], config: Option<&[u8]>) -> io::Result<Vec<f32>> {
    #[cfg(target_os = "macos")]
    {
        imp::decode(codec, sample_rate, channels, packets, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (codec, sample_rate, channels, packets, config);
        Err(io::Error::new(io::ErrorKind::Unsupported, "decoding clips without ffmpeg is only available on macOS"))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::imp::esds_box;

    /// The `esds` box that carries the AudioSpecificConfig to the converter has the lengths its descriptors give:
    /// ES_Descriptor (0x03) at 12 with its ES_ID and flags, DecoderConfigDescriptor (0x04) at 17 with 13 bytes of
    /// fields, and DecoderSpecificInfo (0x05) with the config at 32.
    #[test]
    fn the_esds_box_has_the_setup_data_inside_its_descriptors() {
        let asc = [0x13, 0x08, 0x56, 0xE5];
        let boxed = esds_box(&asc);
        assert_eq!(&boxed[4..8], b"esds");
        assert_eq!(u32::from_be_bytes([boxed[0], boxed[1], boxed[2], boxed[3]]) as usize, boxed.len());
        assert_eq!(boxed[12], 0x03);
        let es_len = boxed[13] as usize;
        assert_eq!(12 + 2 + es_len, boxed.len());
        assert_eq!(boxed[17], 0x04);
        let dcd_len = boxed[18] as usize;
        let dsi_at = 19 + 13;
        assert_eq!(boxed[dsi_at], 0x05);
        assert_eq!(boxed[dsi_at + 1] as usize, asc.len());
        assert_eq!(&boxed[dsi_at + 2..dsi_at + 2 + asc.len()], &asc);
        assert_eq!(19 + dcd_len, dsi_at + 2 + asc.len());
    }
}
