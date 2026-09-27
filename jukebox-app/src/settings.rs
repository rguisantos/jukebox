//! Persisted operator policy. Monetary arithmetic uses integer centavos only.
use ring::{
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Package {
    pub cents: u32,
    pub credits: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub packages: [Package; 3],
    pub coin_cents: u32,
    pub attract_minutes: u32,
    pub free_play: bool,
    pub low_disk_mib: u32,
    pub pin_salt: Vec<u8>,
    pub pin_hash: Vec<u8>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            packages: [
                Package {
                    cents: 100,
                    credits: 1,
                },
                Package {
                    cents: 500,
                    credits: 5,
                },
                Package {
                    cents: 1000,
                    credits: 10,
                },
            ],
            coin_cents: 100,
            attract_minutes: 0,
            free_play: false,
            low_disk_mib: 1024,
            pin_salt: vec![],
            pin_hash: vec![],
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        for p in &self.packages {
            if p.cents == 0 || p.cents > 100_000 || p.credits == 0 || p.credits > 10_000 {
                return Err(
                    "Pacotes: valor entre 1 e 100000 centavos; créditos entre 1 e 10000".into(),
                );
            }
        }
        for pair in self.packages.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.cents <= a.cents
                || b.cents % a.cents != 0
                || (b.credits as u64) < (b.cents / a.cents) as u64 * a.credits as u64
            {
                return Err("Cada pacote deve ser múltiplo do anterior e oferecer ao menos os mesmos créditos por real".into());
            }
        }
        if self.coin_cents == 0
            || self.coin_cents > 100_000
            || self.attract_minutes > 1440
            || !(1..=100_000).contains(&self.low_disk_mib)
        {
            return Err(
                "Valor do pulso, intervalo (0–1440 min) ou limite de disco inválido".into(),
            );
        }
        Ok(())
    }
    /// Largest package first; nested package prices guarantee monotonic conversion.
    pub fn convert(&self, mut cents: u64) -> (u64, u64) {
        let mut credits = 0;
        for p in self.packages.iter().rev() {
            credits += cents / p.cents as u64 * p.credits as u64;
            cents %= p.cents as u64;
        }
        (credits, cents)
    }
    pub fn has_pin(&self) -> bool {
        !self.pin_hash.is_empty()
    }
    pub fn set_pin(&mut self, pin: &str) -> Result<(), String> {
        if !(4..=16).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("Use 4 a 16 letras/números para a senha ou combinação".into());
        }
        let mut salt = [0u8; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| "Falha ao gerar senha")?;
        let mut hash = [0u8; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(100_000).unwrap(),
            &salt,
            pin.as_bytes(),
            &mut hash,
        );
        self.pin_salt = salt.to_vec();
        self.pin_hash = hash.to_vec();
        Ok(())
    }
    pub fn verify_pin(&self, pin: &str) -> bool {
        self.has_pin()
            && pbkdf2::verify(
                pbkdf2::PBKDF2_HMAC_SHA256,
                NonZeroU32::new(100_000).unwrap(),
                &self.pin_salt,
                pin.as_bytes(),
                &self.pin_hash,
            )
            .is_ok()
    }
}

pub fn available_bytes(path: &std::path::Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_bonus_and_remainder() {
        let mut s = Settings::default();
        s.packages[1].credits = 6;
        s.packages[2].credits = 14;
        s.validate().unwrap();
        assert_eq!(s.convert(550), (6, 50));
        assert_eq!(s.convert(1650), (21, 50));
        let mut last = 0;
        for cents in 0..20000 {
            let now = s.convert(cents).0;
            assert!(now >= last);
            last = now;
        }
    }
    #[test]
    fn invalid_packages_rejected() {
        let mut s = Settings::default();
        s.packages[2].credits = 1;
        assert!(s.validate().is_err());
    }
    #[test]
    fn password_is_verified_not_stored() {
        let mut s = Settings::default();
        s.set_pin("qweroi").unwrap();
        assert!(s.verify_pin("qweroi"));
        assert!(!s.verify_pin("qweroq"));
    }
}

/// Paid queue/current playback always wins over automatic selection.
pub fn autoplay_due(
    settings: &Settings,
    operator: bool,
    idle: std::time::Duration,
    since_auto: std::time::Duration,
) -> bool {
    !operator
        && if settings.free_play {
            since_auto >= std::time::Duration::from_millis(250)
        } else {
            settings.attract_minutes > 0
                && idle >= std::time::Duration::from_secs(settings.attract_minutes as u64 * 60)
                && since_auto
                    >= std::time::Duration::from_secs(settings.attract_minutes as u64 * 60)
        }
}
#[cfg(test)]
mod automation_tests {
    use super::*;
    use std::time::Duration as D;
    #[test]
    fn attract_is_disabled_by_default_and_operator_is_never_interrupted() {
        let mut s = Settings::default();
        assert!(!autoplay_due(
            &s,
            false,
            D::from_secs(9999),
            D::from_secs(9999)
        ));
        s.attract_minutes = 2;
        assert!(!autoplay_due(
            &s,
            false,
            D::from_secs(119),
            D::from_secs(200)
        ));
        assert!(autoplay_due(
            &s,
            false,
            D::from_secs(120),
            D::from_secs(200)
        ));
        assert!(!autoplay_due(
            &s,
            true,
            D::from_secs(9999),
            D::from_secs(9999)
        ));
        s.free_play = true;
        assert!(autoplay_due(&s, false, D::ZERO, D::from_secs(1)));
        assert!(!autoplay_due(&s, true, D::ZERO, D::from_secs(1)));
    }
}
