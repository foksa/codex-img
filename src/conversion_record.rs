pub use codex_img_core::conversion_record::*;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{images::Format, palette};
    #[test]
    fn replays_all_conversion_options_through_the_cli_parser() {
        let options = ["a sprite", "--format=png", "--colors=64", "--dither", "--trim=4", "--hard-alpha=40", "--key=auto:12", "--key=#ff0000:25", "--key=blue", "--key-region=top:40%,bottom:30%", "--key-cut=35%", "--key-spread=24", "--trim-density=top,bottom:15%", "--resize=400x600", "--fit=cover", "--no-bleed", "--no-enlarge", "--nearest", "--palette=#000000,#ffffff", "--palette-clean"].map(str::to_string);
        let crate::cli::Command::Run(mut original) = crate::cli::parse(&options).unwrap() else { panic!() };
        original.transform.palette = Some(crate::transform::PaletteFit { colors: palette::parse_list(original.palette.as_ref().unwrap()).unwrap(), clean: true });
        let record = build(Format::Png, &original.encoding, &original.transform);
        let mut args = vec!["same sprite".to_string(), "--format=png".into()];
        args.extend(record["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()));
        let crate::cli::Command::Run(mut replay) = crate::cli::parse(&args).unwrap() else { panic!() };
        replay.transform.palette = Some(crate::transform::PaletteFit { colors: palette::parse_list(replay.palette.as_ref().unwrap()).unwrap(), clean: true });
        assert_eq!(replay.encoding, original.encoding); assert_eq!(replay.transform, original.transform);
        for (format, flags) in [(Format::Webp, vec!["--lossless"]), (Format::Jpeg, vec!["--output-quality=72"])] {
            let mut args = vec!["a picture".into(), format!("--format={}", format.name())]; args.extend(flags.into_iter().map(str::to_string));
            let crate::cli::Command::Run(opts) = crate::cli::parse(&args).unwrap() else { panic!() };
            let record = build(format, &opts.encoding, &opts.transform);
            assert_eq!(record["format"], format.name());
            assert!(record["args"].as_array().unwrap().iter().all(|arg| allowed(arg.as_str().unwrap())));
        }
    }
}
