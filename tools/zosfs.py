"""Read-only access to a ZOS volume: list and extract files without booting ZOS.

    python tools\\zosfs.py ls   volume.img [/path]
    python tools\\zosfs.py cat  volume.img /boot.log
    python tools\\zosfs.py get  volume.img /boot.log boot.log
    python tools\\zosfs.py ls   disk.img --offset-sectors 16384     # volume inside a disk image

`volume.img` is the 16 MiB ZOS partition (see tools\\read-zos-volume.ps1) or any
file containing one. Nothing here ever writes to the image.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import struct
import sys
import zlib

SECTOR = 512
SECTORS = 32768
BLOCK = 4096
BLOCKS = SECTORS // 8
INODES = 64
NODE = 64
NAME = 24
DIRECT = 16
BANKS = (8, 24)
BODY_SECTORS = 9
MAX_FILE = BLOCK * DIRECT


class VolumeError(Exception):
    pass


@dataclass
class Node:
    kind: int  # 0 unused, 1 directory, 2 file
    parent: int
    size: int
    blocks: list[int]
    name: str


class Volume:
    def __init__(self, data: bytes) -> None:
        if len(data) < SECTORS * SECTOR:
            raise VolumeError("image is smaller than a 16 MiB ZOS volume")
        self.data = data
        superblock = data[:SECTOR]
        if superblock[:8] != b"ZOSFS001" or zlib.crc32(superblock[:508]) != struct.unpack_from("<I", superblock, 508)[0]:
            raise VolumeError("no ZOS superblock (wrong offset, or not a ZOS volume)")
        version, sectors, block, inodes = struct.unpack_from("<IIII", superblock, 8)
        if (version, sectors, block, inodes) != (1, SECTORS, BLOCK, INODES):
            raise VolumeError("unsupported ZOS volume geometry")
        banks = [self._bank(index) for index in range(2)]
        valid = [(generation, body) for generation, body in banks if body is not None]
        if not valid:
            raise VolumeError("no valid metadata checkpoint")
        self.generation, body = max(valid, key=lambda item: item[0])
        self.nodes = self._decode(body)

    def _bank(self, index: int) -> tuple[int, bytes | None]:
        start = BANKS[index] * SECTOR
        header = self.data[start:start + SECTOR]
        if header[:8] != b"ZOSBANK1" or zlib.crc32(header[:508]) != struct.unpack_from("<I", header, 508)[0]:
            return 0, None
        generation = struct.unpack_from("<Q", header, 8)[0]
        body = self.data[start + SECTOR:start + SECTOR + BODY_SECTORS * SECTOR]
        if generation == 0 or zlib.crc32(body) != struct.unpack_from("<I", header, 16)[0]:
            return 0, None
        return generation, body

    @staticmethod
    def _decode(body: bytes) -> list[Node]:
        nodes = []
        for index in range(INODES):
            record = body[SECTOR + index * NODE:SECTOR + (index + 1) * NODE]
            kind, length = record[0], record[1]
            parent, size = struct.unpack_from("<HI", record, 2)
            blocks = list(struct.unpack_from("<16H", record, 8))
            if kind not in (0, 1, 2) or length > NAME or size > MAX_FILE:
                raise VolumeError(f"corrupt inode {index}")
            nodes.append(Node(kind, parent, size, blocks, record[40:40 + length].decode("ascii", "replace")))
        if nodes[0].kind != 1:
            raise VolumeError("corrupt root directory")
        return nodes

    def lookup(self, path: str) -> int:
        current = 0
        for part in [p for p in path.split("/") if p]:
            for index, node in enumerate(self.nodes):
                if index != 0 and node.kind and node.parent == current and node.name == part:
                    current = index
                    break
            else:
                raise VolumeError(f"{path}: not found")
        return current

    def children(self, directory: int) -> list[tuple[int, Node]]:
        return [(index, node) for index, node in enumerate(self.nodes)
                if index != 0 and node.kind and node.parent == directory]

    def read(self, index: int) -> bytes:
        node = self.nodes[index]
        if node.kind != 2:
            raise VolumeError("not a file")
        out = bytearray()
        for block in node.blocks[:-(-node.size // BLOCK)]:
            out += self.data[block * BLOCK:(block + 1) * BLOCK]
        return bytes(out[:node.size])

    def listing(self, path: str = "/") -> list[str]:
        index = self.lookup(path)
        node = self.nodes[index]
        if node.kind == 2:
            return [f"{node.size:>8}  {path}"]
        lines = []
        for child, entry in sorted(self.children(index), key=lambda item: item[1].name):
            lines.append(f"{'<dir>' if entry.kind == 1 else entry.size:>8}  {entry.name}{'/' if entry.kind == 1 else ''}")
        return lines


def load(path: Path, offset_sectors: int = 0) -> Volume:
    with path.open("rb") as stream:
        stream.seek(offset_sectors * SECTOR)
        return Volume(stream.read(SECTORS * SECTOR))


def main() -> None:
    parser = argparse.ArgumentParser(description="Read-only ZOS volume reader")
    parser.add_argument("command", choices=["ls", "cat", "get"])
    parser.add_argument("image", type=Path)
    parser.add_argument("path", nargs="?", default="/")
    parser.add_argument("destination", nargs="?", type=Path)
    parser.add_argument("--offset-sectors", type=int, default=0,
                        help="first sector of the volume inside the image (0 for a partition dump)")
    args = parser.parse_args()
    try:
        volume = load(args.image, args.offset_sectors)
        if args.command == "ls":
            print("\n".join(volume.listing(args.path)))
        else:
            data = volume.read(volume.lookup(args.path))
            if args.command == "cat":
                sys.stdout.buffer.write(data)
            else:
                if args.destination is None:
                    parser.error("get needs a destination file")
                args.destination.write_bytes(data)
                print(f"{args.path}: {len(data)} bytes -> {args.destination}")
    except (VolumeError, OSError) as error:
        print(f"zosfs: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
