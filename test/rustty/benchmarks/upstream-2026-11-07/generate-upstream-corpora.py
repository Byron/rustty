"""Generate byte-identical native inputs; never run builds or benchmarks."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import signal
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
SGR = re.compile(rb'\x1b\[([0-9;]+)m')
LINK = re.compile(rb'\x1b\]8;;https://example.com/([0-9]+)\x1b\\([!-~]+)\x1b\]8;;\x1b\\')


def generated(binary, options, size):
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen([str(binary), *options], stdout=subprocess.PIPE, stderr=errors)
        try:
            data = process.stdout.read(size)
            process.stdout.close()
            result = process.wait(timeout=15)
        except BaseException:
            process.kill()
            process.wait()
            raise
        errors.seek(0)
        diagnostic = errors.read().decode(errors='replace')
    if result not in (0, -signal.SIGPIPE) or len(data) != size:
        raise RuntimeError(f'generator returned {result}, {len(data)}/{size} bytes: {diagnostic}')
    return data, diagnostic


def trim(data, kind):
    if kind == 'complete-crlf':
        end = data.rfind(b'\r\n')
        if end < 0:
            raise ValueError('no complete generated record')
        return data[:end + 2]
    try:
        data.decode('utf8')
        return data
    except UnicodeDecodeError as error:
        if error.reason != 'unexpected end of data' or error.end != len(data):
            raise
        return data[:error.start]


def options_map(options):
    return dict(option[2:].split('=', 1) for option in options if option.startswith('--') and '=' in option)


def validate(case, options, data):
    opts = options_map(options)
    action = options[0]
    summary = {'bytes':len(data)}
    if action == '+utf8':
        text = data.decode('utf8')
        assert all(0x4E00 <= ord(cp) <= 0x9FFF or 0x20 <= ord(cp) <= 0x7E for cp in text)
        if float(opts['weight-one']) == 0:
            assert all(0x4E00 <= ord(cp) <= 0x9FFF or cp == ' ' for cp in text)
        summary.update(codepoints=len(text), wide=sum(0x4E00 <= ord(cp) <= 0x9FFF for cp in text), spaces=text.count(' '))
        assert summary['wide'] > 0
        return summary
    lines = data[:-2].split(b'\r\n')
    summary['lines'] = len(lines)
    if action == '+hyperlink':
        uris = set()
        for line in lines:
            matched = LINK.fullmatch(line)
            assert matched is not None
            uri, text = matched.groups()
            assert 0 <= int(uri) < int(opts['uris'])
            assert int(opts['line-min']) <= len(text) <= max(int(opts['line-min']),int(opts['line-max']))
            uris.add(int(uri))
        summary['unique_uris'] = len(uris)
        return summary
    if action == '+styled':
        expected = int(opts.get('sgr-attrs',0))
        summary['sgr_sequences'] = 0
        for line in lines:
            for matched in SGR.finditer(line):
                values = list(map(int, matched.group(1).split(b';')))
                # A single reset that terminates a styled line is not a run SGR.
                if values == [0] and matched.end() == len(line):
                    continue
                index = attrs = 0
                while index < len(values):
                    value = values[index]
                    if value == 0:
                        assert attrs == 0
                    index += 1
                    if value in (38,48):
                        kind = values[index]
                        index += 1
                        if kind == 2:
                            rgb = values[index:index+3]
                            assert len(rgb) == 3 and all(0 <= value <= 255 and value % 51 == 0 for value in rgb)
                            index += 3
                        else:
                            assert kind == 5 and 0 <= values[index] <= 255
                            index += 1
                    attrs += 1
                assert attrs == expected
                summary['sgr_sequences'] += 1
            plain = SGR.sub(b'',line)
            assert int(opts['line-min']) <= len(plain) <= int(opts['line-max'])
            assert all(0x20 <= byte <= 0x7E for byte in plain)
        assert (summary['sgr_sequences'] == 0) == (case['name'] == 'plain')
        return summary
    assert action == '+erase'
    clear = b'\x1b[22J' if opts.get('scroll-clear') == 'true' else b'\x1b[2J'
    extra = int(opts.get('line-clears',0))
    summary['erase_iterations'] = 0
    for line in lines:
        if not line.startswith(clear):
            assert all(0x21 <= byte <= 0x7E for byte in line)
            continue
        summary['erase_iterations'] += 1
        before, redraw = line[len(clear):].split(b'\x1b[H',1)
        assert before.count(b'\x1b[2K') == (extra+1)//2
        assert before.count(b'\x1b[K') == extra//2
        assert redraw.count(b'\x1b[2K') == 1
        assert b'\x1b[31m' in redraw and redraw.endswith(b'\x1b[0m')
    assert summary['erase_iterations'] > 0
    if float(opts.get('erase-rate',1)) == 1:
        assert summary['erase_iterations'] == len(lines)
    else:
        assert summary['erase_iterations'] < len(lines)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('generator', type=Path)
    parser.add_argument('--output', type=Path, default=HERE/'upstream-corpora')
    parser.add_argument('--bytes', type=int)
    parser.add_argument('--names', nargs='*')
    args = parser.parse_args()
    plan = json.loads((HERE/'upstream-workload-plan.json').read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    cases = [case for case in plan['workloads'] if args.names is None or case['name'] in args.names]
    if args.names is not None:
        assert set(args.names) == {case['name'] for case in cases}
    manifest = {'generator':str(args.generator.resolve()),'generator_sha256':hashlib.sha256(args.generator.read_bytes()).hexdigest(),'upstream_range':plan['range'],'status':'generator structural checks only; terminal replay checks pending','corpora':[]}
    for case in cases:
        sizes = case['terminal_sizes'] if case.get('match_generator_dimensions') else [None]
        for dimensions in sizes:
            options = list(case['generator'])
            suffix = ''
            if dimensions:
                cols, rows = dimensions
                options = [option for option in options if not option.startswith(('--rows=','--cols='))]
                options += [f'--rows={rows}',f'--cols={cols}']
                suffix = f'-{cols}x{rows}'
            count = args.bytes if args.bytes is not None else case['bytes']
            raw, diagnostic = generated(args.generator, options, count)
            prefix, _ = generated(args.generator, options, min(count,65536))
            assert raw.startswith(prefix), 'same seed did not reproduce the input prefix'
            data = trim(raw, case['trim'])
            checked = validate(case,options,data)
            name = case['name']+suffix+'.vt'
            (args.output/name).write_bytes(data)
            manifest['corpora'].append({'file':name,'argv':options,'requested_bytes':count,'raw_sha256':hashlib.sha256(raw).hexdigest(),'sha256':hashlib.sha256(data).hexdigest(),'trimmed_bytes':len(raw)-len(data),'checks':checked,'stderr':diagnostic})
            print(name,checked,flush=True)
    (args.output/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')


if __name__ == '__main__':
    main()
