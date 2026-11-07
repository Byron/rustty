from pathlib import Path
patterns = [
    'ordinary Latin office ffi -> ASCII',
    '\x1b[1;34mالعربية\x1b[0m mixed with Latin',
    '\x1b[32mCJK 天地玄黄\x1b[0m and spaces',
    'combining e\u0301 and emoji 👩\u200d💻',
]
data = '\x1b[?2027h' + ''.join(f'\x1b[{row + 1};1H{patterns[row % len(patterns)]}' for row in range(80))
Path(__file__).with_name('shaper-fixture.vt').write_bytes(data.encode())
