pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == 0 || to == 0 || input.is_empty() {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as u64 * u64::from(from);
            let j = (pos / u64::from(to)) as usize;
            if to > from {
                let t = (pos % u64::from(to)) as f32 / to as f32;
                input[j] + (input[(j + 1).min(input.len() - 1)] - input[j]) * t
            } else {
                let end = ((pos + u64::from(from)) / u64::from(to)) as usize;
                input[j..end].iter().sum::<f32>() / (end - j) as f32
            }
        })
        .collect()
}
pub(crate) fn normalize(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0_f32, |p, &s| p.max(s.abs()));
    if peak > 0.0 && peak < 0.5 {
        let gain = (0.7 / peak).min(8.);
        for sample in samples {
            *sample *= gain;
        }
    }
}
pub(crate) fn wav(input: &[f32], rate: u32) -> Option<Vec<u8>> {
    let mut pcm = resample(input, rate, 16000);
    if pcm.len() < 4800 {
        return None;
    }
    normalize(&mut pcm);
    let size = u32::try_from(pcm.len()).ok()?.checked_mul(2)?;
    let mut bytes = Vec::with_capacity(44 + size as usize);
    bytes.extend(b"RIFF");
    bytes.extend(size.checked_add(36)?.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(size.to_le_bytes());
    for s in pcm {
        let s = s.clamp(-1., 1.);
        bytes.extend(((s * if s < 0. { 32768. } else { 32767. }) as i16).to_le_bytes());
    }
    Some(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lengths_and_dc() {
        for from in [48000u32, 44100, 8000] {
            let out = resample(&vec![0.25; from as usize], from, 16000);
            assert_eq!(out.len(), 16000);
            assert!(out.iter().all(|&s| s == 0.25));
        }
        assert_eq!(resample(&[0., 1.], 8000, 16000), [0., 0.5, 1., 1.]);
    }
    #[test]
    fn gain_and_wav() {
        let mut x = [0.01, -0.01];
        normalize(&mut x);
        assert_eq!(x, [0.08, -0.08]);
        let mut x = [0.5, -1.];
        normalize(&mut x);
        assert_eq!(x, [0.5, -1.]);
        assert!(wav(&[1.; 200], 16000).is_none());
        let bytes = wav(&vec![-1.; 4800], 16000).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(&bytes[20..24], &[1, 0, 1, 0]);
        assert_eq!(&bytes[24..28], &16000u32.to_le_bytes());
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(&bytes[40..44], &9600u32.to_le_bytes());
        assert_eq!(&bytes[44..46], &[0, 128]);
    }
}
