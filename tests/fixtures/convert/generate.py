"""Freeze local conversion outputs BEFORE extraction. Never run automatically in tests."""
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import zlib

folder = Path(__file__).resolve().parent
binary = Path(sys.argv[1]).resolve()
def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
width, height = 48, 40
rows = bytearray()
for y in range(height):
    rows.append(0)
    for x in range(width):
        pixel = (17, 29, 43, 0)
        if 3 <= x < 45 and 4 <= y < 35:
            pixel = (12 + x, 150 + y, 210 - y, 255)
        if 12 <= x < 36 and 9 <= y < 27:
            pixel = ((x * 11 + y * 3) % 256, (y * 9 + x * 3) % 256, (x * 7 + y * 11) % 256, 255)
        if (x, y) in [(8, 36), (27, 37), (17, 2)]:
            pixel = (200, 20, 60, 12 if y == 2 else 255)
        if x in [11, 36] and 9 <= y < 27:
            pixel = (220, 40, 60, [8, 40, 90, 140, 210][y % 5])
        rows.extend(pixel)
source = folder / 'source.png'
source.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))
env = {**os.environ, 'CODEX_IMG_EVENTS': 'off'}
for fmt in ['jpeg', 'webp']:
    subprocess.run([str(binary), 'convert', str(source), '-o', str(folder / f'source.{fmt}'), f'--format={fmt}', '--quiet'], env=env, check=True, capture_output=True)
palette = '--palette=#000000,#FFFFFF,#DD3344,#116688,#22BBCC,#886633'
cases = [
    ('png', []), ('trim', ['--trim']), ('trim-padding', ['--trim=3']),
    ('trim-density', ['--trim', '--trim-density=0.15']),
    ('trim-density-all', ['--trim', '--trim-density=all:0.25']),
    ('hard-alpha', ['--hard-alpha']), ('hard-alpha-threshold', ['--hard-alpha=127']),
    ('key-auto', ['--key=auto:24']), ('key-named', ['--key=cyan']),
    ('key-rgb', ['--key=#20B4C8:48']),
    ('key-region', ['--key=cyan', '--key-region=bottom:60%']),
    ('key-cut', ['--key=cyan', '--key-region=bottom:60%', '--key-cut=40%']),
    ('key-spread', ['--key=cyan', '--key-region=all:30%', '--key-spread=24']),
    ('key-multiple', ['--key=cyan', '--key=blue']),
    ('palette', [palette]), ('palette-clean', [palette, '--palette-clean']),
    ('colors', ['--colors=16']), ('colors-dither', ['--colors=16', '--dither']),
    ('resize-width', ['--resize=24x']), ('resize-height', ['--resize=x24']),
    *[(f'resize-{fit}', ['--resize=24x32', f'--fit={fit}']) for fit in ['inside', 'cover', 'contain', 'fill']],
    ('no-enlarge', ['--resize=96x80', '--no-enlarge']),
    ('nearest-up', ['--resize=96x80', '--nearest']),
    ('nearest-down', ['--resize=24x20', '--nearest']),
    ('no-bleed', ['--no-bleed']),
    ('combined', ['--hard-alpha=40', '--key=cyan', '--key-region=bottom:50%', '--key-cut=30%', '--key-spread=16', '--trim=2', '--trim-density=bottom:0.15', palette, '--palette-clean', '--resize=32x32', '--fit=contain', '--nearest']),
    ('jpeg', ['--format=jpeg']), ('jpeg-quality', ['--format=jpeg', '--output-quality=63']),
    ('webp', ['--format=webp']), ('webp-quality', ['--format=webp', '--output-quality=63']),
    ('webp-lossless', ['--format=webp', '--lossless']),
    ('palette-webp', [palette, '--format=webp', '--lossless']),
]
records = []
(folder / 'golden').mkdir(exist_ok=True)
for name, args in cases:
    fmt = next((arg.split('=')[1] for arg in args if arg.startswith('--format=')), 'png')
    record = {'name': name, 'input': 'source.png', 'output': f'golden/{name}.{fmt}', 'args': args}
    records.append(record)
for fmt in ['jpeg', 'webp']:
    records.append({'name': f'{fmt}-input', 'input': f'source.{fmt}', 'output': f'golden/{fmt}-input.png', 'args': ['--format=png', '--resize=24x']})
for record in records:
    command = [str(binary), 'convert', str(folder / record['input']), '-o', str(folder / record['output']), '--quiet', '--json', *record['args']]
    if any(arg.startswith('--key=') for arg in record['args']):
        record['mask'] = f"golden/{record['name']}-mask.png"
        command.append(f"--mask-out={folder / record['mask']}")
    report = json.loads(subprocess.check_output(command, env=env))
    record['report'] = {key: value for key, value in report.items() if key not in ['path', 'input', 'durationMs', 'maskPath']}
(folder / 'cases.json').write_text(json.dumps(records, indent=2) + '\n')
print(f'Froze {len(records)} cases using {binary} ({binary.stat().st_size} bytes)')
