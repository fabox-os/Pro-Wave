//! Class IDs as the 32-digit hexadecimal strings `.vstpreset` files and the SDK's
//! `FUID::toString` use. The in-memory byte order of a `TUID` differs between Windows and other
//! platforms; the string does not.

use vst3::Steinberg::TUID;

pub fn to_string(tuid: &TUID) -> String {
    let [a, b, c, d] = words(tuid);
    format!("{a:08X}{b:08X}{c:08X}{d:08X}")
}

fn words(tuid: &TUID) -> [u32; 4] {
    let b: [u8; 16] = tuid.map(|byte| byte as u8);
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    if cfg!(target_os = "windows") {
        let le16 = |i: usize| u32::from(u16::from_le_bytes([b[i], b[i + 1]]));
        [
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (le16(4) << 16) | le16(6),
            be(8),
            be(12),
        ]
    } else {
        [be(0), be(4), be(8), be(12)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_is_the_same_on_every_platform() {
        let tuid = vst3::uid(0x84E8DE5F, 0x92554F53, 0x96FAE413, 0x3C935A18);
        assert_eq!(to_string(&tuid), "84E8DE5F92554F5396FAE4133C935A18");
    }
}
