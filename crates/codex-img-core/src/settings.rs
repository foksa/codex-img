//! Local settings only. Palette names are resolved by the caller, outside core.
use crate::{error::{Error, Result}, images::{Encoding, Format}, palette::Rgb, transform::{self, Fit, Key, PaletteFit, Resize, Transform}};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings { pub format: Format, pub encoding: Encoding, pub transform: Transform }
impl Settings {
    pub fn parse(args: &[String], palette: impl Fn(&str) -> Result<Vec<Rgb>>) -> Result<Self> {
        let mut result = Self { format: Format::Png, encoding: Encoding::default(), transform: Transform::default() };
        let mut swatch = None;
        let mut clean = false;
        for arg in args {
            let (flag, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(a,b)| (a, Some(b)));
            let value = || inline.ok_or_else(|| Error::usage(format!("{flag} needs a value.")));
            match flag {
                "--format" => result.format = Format::parse(value()?).ok_or_else(|| Error::usage("--format must be one of: png, jpeg, webp"))?,
                "--colors" => result.encoding.colors = Some(value()?.parse().ok().filter(|n| (2..=256).contains(n)).ok_or_else(|| Error::usage("--colors must be an integer from 2 to 256."))?),
                "--output-quality" => result.encoding.quality = Some(value()?.parse().ok().filter(|n| (1..=100).contains(n)).ok_or_else(|| Error::usage("--output-quality must be an integer from 1 to 100."))?),
                "--trim" => result.transform.trim = Some(inline.map(transform::parse_trim_padding).transpose()?.unwrap_or(0)),
                "--hard-alpha" => result.transform.hard_alpha = Some(inline.map(transform::parse_hard_alpha).transpose()?.unwrap_or(transform::FAINT_ALPHA)),
                "--resize" => result.transform.resize = Some(Resize::parse(value()?)?),
                "--fit" => result.transform.fit = Some(Fit::parse(value()?)?),
                "--key" => result.transform.keys.push(Key::parse(value()?)?),
                "--key-region" => result.transform.key_region = Some(transform::Region::parse(value()?)?),
                "--key-spread" => result.transform.key_spread = Some(transform::parse_key_spread(value()?)?),
                "--key-cut" => result.transform.key_cut = Some(inline.map(transform::parse_key_cut).transpose()?.unwrap_or(transform::KEY_CUT)),
                "--trim-density" => result.transform.trim_density = Some(transform::Density::parse(value()?)?),
                "--palette" => swatch = Some(value()?.to_owned()),
                "--dither" | "--lossless" | "--palette-clean" | "--no-enlarge" | "--nearest" | "--no-bleed" if inline.is_none() => match flag {
                    "--dither" => result.encoding.dither = true, "--lossless" => result.encoding.lossless = true,
                    "--palette-clean" => clean = true, "--no-enlarge" => result.transform.no_enlarge = true,
                    "--nearest" => result.transform.nearest = true, _ => result.transform.no_bleed = true,
                },
                _ => return Err(Error::usage(format!("Unknown conversion setting: {arg}"))),
            }
        }
        if clean && swatch.is_none() { return Err(Error::usage("--palette-clean only applies with --palette.")); }
        if let Some(swatch) = swatch { result.transform.palette = Some(PaletteFit { colors: palette(&swatch)?, clean }); }
        result.encoding.check(Some(result.format))?;
        result.transform.check_output(Some(result.format), &result.encoding)?;
        result.transform.check()?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_nonlocal_arguments_and_invalid_combinations() {
        for args in [vec!["--force"], vec!["--output=x.png"], vec!["--via-responses"], vec!["--palette-clean"], vec!["--colors=1"], vec!["--output-quality=101"], vec!["--dither"], vec!["--format=jpeg", "--lossless"], vec!["--nearest=true"]] {
            assert!(Settings::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>(), |_| unreachable!()).is_err());
        }
    }
    #[test]
    fn every_frozen_case_roundtrips_its_manifest_record() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("../../../tests/fixtures/convert/cases.json")).unwrap();
        for case in cases {
            let args = case["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()).collect::<Vec<_>>();
            let settings = Settings::parse(&args, crate::palette::parse_list).unwrap();
            let record = crate::conversion_record::build(settings.format, &settings.encoding, &settings.transform);
            let mut recorded = vec![format!("--format={}", record["format"].as_str().unwrap())];
            recorded.extend(record["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()));
            assert_eq!(Settings::parse(&recorded, crate::palette::parse_list).unwrap(), settings, "{}", case["name"]);
        }
    }
}
