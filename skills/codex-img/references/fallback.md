# When `codex-img` isn't installed

When `command -v codex-img` finds nothing, use the bundled fallback, `scripts/codex_img.py` in this skill's directory (Python 3.9+, standard library only). It covers the common generation options with the same names, exit codes and `--json` output:

```sh
python3 <skill-dir>/scripts/codex_img.py "<prompt>" -o <path>.png --json
```

**Supported:** `-o`, `-i`, `-a`, `-s`, `-q`, `-b`, `-n`, `--view` (built-in views only), `--style-ref`, `--character-ref`, `--composition-ref`, `--json`, `--quiet`, and `status`.

**Refused before any quota is spent:**
- **Any output format but PNG:** `-f`, and any `-o` extension other than `.png`.
- **Processing:** `-c`, `--dither`, `--output-quality`, `--lossless`, `--trim`, `--resize`, `--fit`, `--hard-alpha`, `--no-enlarge` and `--no-bleed`.
- **Presets and palettes:** `--style`, `--character` and `--palette`, as well as the project's own views.
- **Other options:** `--manifest`, `--via-responses` and `--model`.
- **Subcommands:** `convert`, `sheet`, `batch`, `tile` and `presets`.

If the user wants JPEG or WebP, exact pixel dimensions, trimming or a palette, generate a PNG and then process it yourself with `sips`, ImageMagick or Pillow. Say that you did. For exact dimensions on macOS: `sips -z H W in.png --out out.png`.

If `python3` is missing as well, tell the user rather than trying another image service. They can install the binary from the project's releases page.
