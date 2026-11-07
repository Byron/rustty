"""Reconstruct the exact baseline inputs from the committed protocols archive."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path


def reconstruct(archive, attribution):
    audit = json.loads(attribution.read_text())
    # Runner serialization sorts object keys; retain the recorded execution order.
    records = sorted(audit['records'], key=lambda row: row['current_directory'])
    ids = [row['id'] for row in records if not row['id'].endswith(('/scalar', '/chunks'))]
    with gzip.open(archive, 'rt') as stream:
        next(stream)
        cases = {row['request']['id']: row['request'] for row in map(json.loads, stream)}
    requests = [{'request': json.loads(json.dumps(cases[name], sort_keys=True)), 'covers': []}
                for name in ids]
    assert len(requests) == 59
    data = (json.dumps(requests, indent=2) + '\n').encode()
    assert hashlib.sha256(data).hexdigest() == audit['supporting_sha256']['final-protocol-residual-fixtures.json']
    return data


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path, help='Committed test/rustty/fixtures/protocols.jsonl.gz')
    parser.add_argument('attribution', type=Path, help='final-protocol-residual-attribution.json')
    parser.add_argument('output', type=Path, help='New fixture JSON path')
    args = parser.parse_args()
    data = reconstruct(args.archive, args.attribution)
    with args.output.open('xb') as stream:
        stream.write(data)
