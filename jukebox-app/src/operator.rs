//! Operator-form validation is kept outside the UI callback implementation.
use crate::{
    settings::{Package, Settings},
    ConfigData,
};
/// Owned input at the UI boundary. Storage commands never carry Slint types.
#[derive(Debug)]
pub struct SettingsInput {
    pub base_cents: String,
    pub base_credits: String,
    pub pack_cents: String,
    pub pack_credits: String,
    pub large_cents: String,
    pub large_credits: String,
    pub coin_cents: String,
    pub attract_minutes: String,
    pub low_disk_mib: String,
    pub free_play: bool,
}
impl From<ConfigData> for SettingsInput {
    fn from(form: ConfigData) -> Self {
        Self {
            base_cents: form.base_cents.to_string(),
            base_credits: form.base_credits.to_string(),
            pack_cents: form.pack_cents.to_string(),
            pack_credits: form.pack_credits.to_string(),
            large_cents: form.large_cents.to_string(),
            large_credits: form.large_credits.to_string(),
            coin_cents: form.coin_cents.to_string(),
            attract_minutes: form.attract_minutes.to_string(),
            low_disk_mib: form.low_disk_mib.to_string(),
            free_play: form.free_play,
        }
    }
}
pub fn form(s: &Settings) -> ConfigData {
    ConfigData {
        base_cents: s.packages[0].cents.to_string().into(),
        base_credits: s.packages[0].credits.to_string().into(),
        pack_cents: s.packages[1].cents.to_string().into(),
        pack_credits: s.packages[1].credits.to_string().into(),
        large_cents: s.packages[2].cents.to_string().into(),
        large_credits: s.packages[2].credits.to_string().into(),
        coin_cents: s.coin_cents.to_string().into(),
        attract_minutes: s.attract_minutes.to_string().into(),
        low_disk_mib: s.low_disk_mib.to_string().into(),
        free_play: s.free_play,
    }
}
pub fn parse(f: SettingsInput, mut current: Settings) -> Result<Settings, String> {
    fn number(s: &str) -> Result<u32, String> {
        s.trim()
            .parse()
            .map_err(|_| "Preencha os campos numéricos com inteiros".into())
    }
    current.packages = [
        Package {
            cents: number(&f.base_cents)?,
            credits: number(&f.base_credits)?,
        },
        Package {
            cents: number(&f.pack_cents)?,
            credits: number(&f.pack_credits)?,
        },
        Package {
            cents: number(&f.large_cents)?,
            credits: number(&f.large_credits)?,
        },
    ];
    current.coin_cents = number(&f.coin_cents)?;
    current.attract_minutes = number(&f.attract_minutes)?;
    current.low_disk_mib = number(&f.low_disk_mib)?;
    current.free_play = f.free_play;
    current.validate()?;
    Ok(current)
}
