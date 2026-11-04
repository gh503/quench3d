#!/usr/bin/env python3
"""Compare every canonical state against frozen x86_64 Linux baseline hashes."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def frames(root, cases):
    result = {}
    for name, expected_count in cases.items():
        records = []
        with (root / (name + '.bin')).open('rb') as stream:
            while True:
                header = stream.read(8)
                if not header:
                    break
                if len(header) != 8:
                    raise ValueError(f'{name}: truncated frame header')
                length = struct.unpack('<Q', header)[0]
                digest = hashlib.sha256()
                remaining = length
                while remaining:
                    block = stream.read(min(remaining, 1024 * 1024))
                    if not block:
                        raise ValueError(f'{name}: truncated state {len(records)+1}')
                    digest.update(block)
                    remaining -= len(block)
                records.append({'length': length, 'sha256': digest.hexdigest()})
        if len(records) != expected_count:
            raise ValueError(f'{name}: frames {len(records)}, expected {expected_count}')
        result[name] = records
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--manifest', type=Path, default=Path(__file__).with_name('default_bits_193.json'))
    args = parser.parse_args()
    expected = json.loads(args.manifest.read_text())
    actual = frames(args.root, {name: len(records) for name, records in expected['cases'].items()})
    for name, records in actual.items():
        for step, (left, right) in enumerate(zip(records, expected['cases'][name]), 1):
            if left != right:
                raise ValueError(f'{name} step={step}: canonical length/SHA256 mismatch')
    print(f'DEFAULT_BITS PASS states={sum(map(len, actual.values()))} payload_bytes={sum(r["length"] for records in actual.values() for r in records)} platform={expected["platform"]}')


if __name__ == '__main__':
    main()
