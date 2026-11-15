"""Build the ZOS GPT/FAT32 disk image using only the Python standard library."""

from __future__ import annotations

import math
import struct
import uuid
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import BinaryIO

SECTOR = 512
SECTORS = 128 * 1024 * 1024 // SECTOR
BIOS_START, BIOS_END = 2048, 4095
ESP_START, ESP_END = 4096, SECTORS - 34
ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"


def image_path(name: str) -> Path:
    if not name or any(ch in name for ch in "\\/:") or Path(name).name != name or not name.endswith(".img"):
        raise ValueError("Image name must be a plain .img filename")
    destination = (BUILD / name).resolve()
    if destination.parent != BUILD.resolve() or BUILD.resolve() != BUILD or (BUILD / name).is_symlink():
        raise ValueError("Images must be regular files directly inside build")
    if destination.exists() and not destination.is_file():
        raise ValueError("Image destination is not a regular file")
    return destination


def crc(data: bytes | bytearray) -> int:
    return zlib.crc32(data) & 0xFFFFFFFF


def gpt_header(current: int, backup: int, entries_lba: int, entries: bytes, total: int = SECTORS) -> bytes:
    header = bytearray(SECTOR)
    struct.pack_into(
        "<8sIIIIQQQQ16sQIII",
        header,
        0,
        b"EFI PART",
        0x10000,
        92,
        0,
        0,
        current,
        backup,
        34,
        total - 34,
        uuid.uuid5(uuid.NAMESPACE_DNS, "zos.disk").bytes_le,
        entries_lba,
        128,
        128,
        crc(entries),
    )
    struct.pack_into("<I", header, 16, crc(header[:92]))
    return bytes(header)


def write_gpt(stream: BinaryIO) -> None:
    stream.truncate(SECTORS * SECTOR)
    mbr = bytearray(SECTOR)
    struct.pack_into(
        "<B3sB3sII", mbr, 446, 0, b"\x00\x02\x00", 0xEE, b"\xff\xff\xff", 1, SECTORS - 1
    )
    mbr[510:512] = b"\x55\xaa"
    stream.seek(0)
    stream.write(mbr)
    entries = bytearray(128 * 128)
    for index, (kind, first, last, name) in enumerate(
        [
            ("21686148-6449-6E6F-744E-656564454649", BIOS_START, BIOS_END, "ZOS BIOS"),
            ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", ESP_START, ESP_END, "ZOS ESP"),
        ]
    ):
        struct.pack_into(
            "<16s16sQQQ72s",
            entries,
            index * 128,
            uuid.UUID(kind).bytes_le,
            uuid.uuid5(uuid.NAMESPACE_DNS, name).bytes_le,
            first,
            last,
            0,
            name.encode("utf-16le").ljust(72, b"\0"),
        )
    for lba in (2, SECTORS - 33):
        stream.seek(lba * SECTOR)
        stream.write(entries)
    for current, backup, entries_lba in [(1, SECTORS - 1, 2), (SECTORS - 1, 1, SECTORS - 33)]:
        stream.seek(current * SECTOR)
        stream.write(gpt_header(current, backup, entries_lba, entries))


def short_name(name: str, used: set[bytes]) -> bytes:
    base, dot, extension = name.upper().rpartition(".")
    if not dot:
        base, extension = name.upper(), ""
    allowed = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
    if not base or any(ch not in allowed for ch in base + extension):
        raise ValueError(f"Unsupported image filename: {name}")
    candidate = base[:8].ljust(8) + extension[:3].ljust(3)
    if len(base) <= 8 and len(extension) <= 3 and candidate.encode() not in used:
        result = candidate.encode()
    else:
        for number in range(1, 100000):
            suffix = f"~{number}"
            candidate = (base[: 8 - len(suffix)] + suffix).ljust(8) + extension[:3].ljust(3)
            if candidate.encode() not in used:
                result = candidate.encode()
                break
        else:
            raise ValueError("8.3 alias space exhausted")
    used.add(result)
    return result


def long_entries(name: str, alias: bytes) -> bytes:
    checksum = 0
    for byte in alias:
        checksum = (((checksum & 1) << 7) + (checksum >> 1) + byte) & 0xFF
    units = list(struct.unpack(f"<{len(name.encode('utf-16le')) // 2}H", name.encode("utf-16le")))
    units.append(0)
    units.extend([0xFFFF] * ((-len(units)) % 13))
    result = bytearray()
    count = len(units) // 13
    for index in range(count, 0, -1):
        chunk = units[(index - 1) * 13 : index * 13]
        result.extend(
            struct.pack(
                "<B10sBBB12sH4s",
                index | (0x40 if index == count else 0),
                struct.pack("<5H", *chunk[:5]),
                0x0F,
                0,
                checksum,
                struct.pack("<6H", *chunk[5:11]),
                0,
                struct.pack("<2H", *chunk[11:]),
            )
        )
    return bytes(result)


def directory_entry(alias: bytes, cluster: int, size: int, directory: bool) -> bytes:
    entry = bytearray(32)
    entry[:11] = alias
    entry[11] = 0x10 if directory else 0x20
    struct.pack_into("<H", entry, 16, 33)  # 1980-01-01, for reproducible images.
    struct.pack_into("<H", entry, 18, 33)
    struct.pack_into("<H", entry, 20, cluster >> 16)
    struct.pack_into("<H", entry, 24, 33)
    struct.pack_into("<H", entry, 26, cluster & 0xFFFF)
    struct.pack_into("<I", entry, 28, size)
    return bytes(entry)


@dataclass
class Node:
    name: str
    data: bytes | None = None
    parent: Node | None = None
    children: dict[str, Node] = field(default_factory=dict)
    alias: bytes = b""
    cluster: int = 0
    count: int = 0

    @property
    def directory(self) -> bool:
        return self.data is None


class Fat32:
    def __init__(self) -> None:
        self.root = Node("")
        self.total_sectors = ESP_END - ESP_START + 1
        self.fat_sectors = math.ceil(self.total_sectors * 4 / (SECTOR + 8))
        self.clusters = self.total_sectors - 32 - 2 * self.fat_sectors
        if self.clusters < 65525:
            raise ValueError("Volume is too small for FAT32")
        self.fat = bytearray(self.fat_sectors * SECTOR)
        struct.pack_into("<II", self.fat, 0, 0x0FFFFFF8, 0x0FFFFFFF)
        self.next_cluster = 2

    def add(self, path: str, data: bytes) -> None:
        parts = path.split("/")
        if any(part in ("", ".", "..") for part in parts):
            raise ValueError("Invalid image-relative path")
        node = self.root
        for part in parts[:-1]:
            node = node.children.setdefault(part, Node(part, parent=node))
            if not node.directory:
                raise ValueError("File used as a directory")
        if parts[-1] in node.children:
            raise ValueError("Duplicate image path")
        node.children[parts[-1]] = Node(parts[-1], data=data, parent=node)

    def allocate(self, node: Node) -> list[Node]:
        used: set[bytes] = set()
        children = sorted(node.children.values(), key=lambda child: child.name)
        for child in children:
            child.alias = short_name(child.name, used)
        if node.directory:
            size = (64 if node.parent else 0) + 32
            size += sum(len(long_entries(child.name, child.alias)) + 32 for child in children)
        else:
            size = len(node.data)
        node.count = (size + SECTOR - 1) // SECTOR
        if node.count:
            if self.next_cluster + node.count > self.clusters + 2:
                raise ValueError("EFI System Partition is full")
            node.cluster = self.next_cluster
            for cluster in range(node.cluster, node.cluster + node.count):
                next_cluster = cluster + 1 if cluster + 1 < node.cluster + node.count else 0x0FFFFFFF
                struct.pack_into("<I", self.fat, cluster * 4, next_cluster)
            self.next_cluster += node.count
        nodes = [node]
        for child in children:
            nodes.extend(self.allocate(child))
        return nodes

    def directory_data(self, node: Node) -> bytes:
        data = bytearray()
        if node.parent:
            data.extend(directory_entry(b".          ", node.cluster, 0, True))
            parent_cluster = node.parent.cluster if node.parent.parent else 0
            data.extend(directory_entry(b"..         ", parent_cluster, 0, True))
        for child in sorted(node.children.values(), key=lambda child: child.name):
            data.extend(long_entries(child.name, child.alias))
            data.extend(
                directory_entry(
                    child.alias, child.cluster, 0 if child.directory else len(child.data), child.directory
                )
            )
        data.extend(bytes(32))
        return bytes(data)

    def write(self, stream: BinaryIO) -> None:
        nodes = self.allocate(self.root)
        boot = bytearray(SECTOR)
        boot[:11] = b"\xeb\x58\x90ZOS     "
        struct.pack_into("<HBHBHHBHHHII", boot, 11, 512, 1, 32, 2, 0, 0, 0xF8, 0, 63, 255,
                         ESP_START, self.total_sectors)
        struct.pack_into("<IHHIHH", boot, 36, self.fat_sectors, 0, 0, self.root.cluster, 1, 6)
        struct.pack_into("<BBBI11s8s", boot, 64, 0x80, 0, 0x29, 0x5A4F5301, b"ZOMBIE OS  ", b"FAT32   ")
        boot[510:512] = b"\x55\xaa"
        fsinfo = bytearray(SECTOR)
        struct.pack_into("<I", fsinfo, 0, 0x41615252)
        struct.pack_into("<III", fsinfo, 484, 0x61417272,
                         self.clusters - (self.next_cluster - 2), self.next_cluster)
        struct.pack_into("<I", fsinfo, 508, 0xAA550000)
        for sector, data in [(0, boot), (1, fsinfo), (6, boot), (7, fsinfo)]:
            stream.seek((ESP_START + sector) * SECTOR)
            stream.write(data)
        for copy in range(2):
            stream.seek((ESP_START + 32 + copy * self.fat_sectors) * SECTOR)
            stream.write(self.fat)
        data_start = ESP_START + 32 + 2 * self.fat_sectors
        for node in nodes:
            if not node.count:
                continue
            stream.seek((data_start + node.cluster - 2) * SECTOR)
            stream.write(self.directory_data(node) if node.directory else node.data)


def create_image(destination: Path, files: dict[str, bytes]) -> None:
    if destination.resolve() != image_path(destination.name):
        raise ValueError("Image destination must be inside the project build directory")
    volume = Fat32()
    for name, data in files.items():
        volume.add(name, data)
    destination.parent.mkdir(exist_ok=True)
    with destination.open("wb") as stream:
        write_gpt(stream)
        volume.write(stream)
