"""Format only dedicated build/data*.img volumes for the ZOS simple filesystem."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import struct
import uuid
import zlib

from mkimage import BUILD, crc, gpt_header, image_path

SECTOR = 512
SECTORS = 32768
BANKS = (8, 24)
BODY_SECTORS = 9


def data_path(name: str) -> Path:
    if re.fullmatch(r"data(?:-[A-Za-z0-9_-]+)?\.img", name) is None:
        raise ValueError("Data volume must be data.img or data-<name>.img inside build")
    return image_path(name)


def superblock() -> bytes:
    sector = bytearray(SECTOR)
    struct.pack_into("<8sIIII", sector, 0, b"ZOSFS001", 1, SECTORS, 4096, 64)
    struct.pack_into("<I", sector, 508, zlib.crc32(sector[:508]))
    return bytes(sector)


def metadata() -> tuple[bytes, bytes]:
    body = bytearray(BODY_SECTORS * SECTOR)
    body[0] = 255
    body[SECTOR] = 1
    header = bytearray(SECTOR)
    struct.pack_into("<8sQI", header, 0, b"ZOSBANK1", 1, zlib.crc32(body))
    struct.pack_into("<I", header, 508, zlib.crc32(header[:508]))
    return bytes(header), bytes(body)


def volume_image() -> bytes:
    """A complete, freshly formatted 16 MiB ZOS volume."""
    header, body = metadata()
    volume = bytearray(SECTORS * SECTOR)
    volume[:SECTOR] = superblock()
    volume[BANKS[0] * SECTOR:(BANKS[0] + 1) * SECTOR] = header
    volume[(BANKS[0] + 1) * SECTOR:(BANKS[0] + 1 + BODY_SECTORS) * SECTOR] = body
    return bytes(volume)


def validate_volume(stream, name: str, offset: int = 0) -> None:
    stream.seek(offset)
    if stream.read(SECTOR) != superblock():
        raise ValueError(f"{name}: invalid filesystem geometry/checksum; refusing to reformat")
    valid = False
    for bank in BANKS:
        stream.seek(offset + bank * SECTOR)
        header, body = stream.read(SECTOR), stream.read(BODY_SECTORS * SECTOR)
        if header[:8] == b"ZOSBANK1" and struct.unpack_from("<Q", header, 8)[0] > 0 \
                and zlib.crc32(header[:508]) == struct.unpack_from("<I", header, 508)[0] \
                and zlib.crc32(body) == struct.unpack_from("<I", header, 16)[0]:
            valid = True
    if not valid:
        raise ValueError(f"{name}: no valid metadata checkpoint; refusing to reformat")


def validate_disk(path: Path) -> None:
    if path.stat().st_size == STICK_SECTORS * SECTOR:
        validate_gpt_disk(path)
        return
    if path.stat().st_size != SECTORS * SECTOR:
        raise ValueError(f"{path.name}: expected a 16 MiB ZOS data volume; refusing to reformat")
    with path.open("rb") as stream:
        validate_volume(stream, path.name)


# A 64 MiB test "stick": a GPT with a decoy partition and the ZOS data partition at a
# non-zero offset. Every other sector carries a recognisable pattern so that any write
# outside the ZOS partition is detectable.
ZOS_PARTITION_TYPE = uuid.UUID("f2ac67b5-91fa-4c46-a324-95b456c3829d")
BASIC_DATA_TYPE = uuid.UUID("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
STICK_SECTORS = 64 * 1024 * 1024 // SECTOR
DECOY_FIRST, DECOY_LAST = 2048, 8191
ZOS_FIRST = 16384
ZOS_LAST = ZOS_FIRST + SECTORS - 1


def _partition_entries(zos: bool) -> bytes:
    entries = bytearray(128 * 128)
    layout = [(BASIC_DATA_TYPE, DECOY_FIRST, DECOY_LAST, "DECOY")]
    if zos:
        layout.append((ZOS_PARTITION_TYPE, ZOS_FIRST, ZOS_LAST, "ZOS DATA"))
    for index, (kind, first, last, label) in enumerate(layout):
        struct.pack_into("<16s16sQQQ72s", entries, index * 128, kind.bytes_le,
                         uuid.uuid5(uuid.NAMESPACE_DNS, "zos.test." + label).bytes_le, first, last, 0,
                         label.encode("utf-16le").ljust(72, b"\0"))
    return bytes(entries)


def format_gpt_disk(name: str, *, force: bool = False, zos: bool = True, formatted: bool = True) -> Path:
    """zos=False omits the ZOS partition; formatted=False leaves it unformatted (foreign data)."""
    path = data_path(name)
    BUILD.mkdir(exist_ok=True)
    if path.exists() and not force:
        raise FileExistsError(f"{path.name} already exists; explicit mkfs --force is required")
    image = bytearray(STICK_SECTORS * SECTOR)
    for lba in range(STICK_SECTORS):
        image[lba * SECTOR:(lba + 1) * SECTOR] = struct.pack("<QQ", 0x5A4F530000000000 + lba, lba ^ 0xFFFF) * 32
    mbr = bytearray(SECTOR)
    struct.pack_into("<B3sB3sII", mbr, 446, 0, b"\x00\x02\x00", 0xEE, b"\xff\xff\xff", 1, STICK_SECTORS - 1)
    mbr[510:512] = b"\x55\xaa"
    image[:SECTOR] = mbr
    entries = _partition_entries(zos)
    image[2 * SECTOR:2 * SECTOR + len(entries)] = entries
    backup_entries = (STICK_SECTORS - 33) * SECTOR
    image[backup_entries:backup_entries + len(entries)] = entries
    for current, backup, entries_lba in ((1, STICK_SECTORS - 1, 2), (STICK_SECTORS - 1, 1, STICK_SECTORS - 33)):
        image[current * SECTOR:(current + 1) * SECTOR] = gpt_header(
            current, backup, entries_lba, entries, STICK_SECTORS)
    if zos and formatted:
        image[ZOS_FIRST * SECTOR:(ZOS_LAST + 1) * SECTOR] = volume_image()
    with path.open("wb") as stream:
        stream.write(image)
        stream.flush()
        os.fsync(stream.fileno())
    validate_gpt_disk(path)
    return path


def validate_gpt_disk(path: Path) -> None:
    if path.stat().st_size != STICK_SECTORS * SECTOR:
        raise ValueError(f"{path.name}: expected a 64 MiB GPT test stick")
    with path.open("rb") as stream:
        stream.seek(SECTOR)
        header = bytearray(stream.read(SECTOR))
        stored = struct.unpack_from("<I", header, 16)[0]
        struct.pack_into("<I", header, 16, 0)
        if header[:8] != b"EFI PART" or crc(header[:92]) != stored:
            raise ValueError(f"{path.name}: invalid GPT header; refusing to reuse")
        stream.seek(2 * SECTOR)
        entries = stream.read(128 * 128)
        if crc(entries) != struct.unpack_from("<I", header, 88)[0]:
            raise ValueError(f"{path.name}: invalid GPT entry array; refusing to reuse")
        for index in range(128):
            entry = entries[index * 128:(index + 1) * 128]
            if entry[:16] == ZOS_PARTITION_TYPE.bytes_le:
                first = struct.unpack_from("<Q", entry, 32)[0]
                stream.seek(first * SECTOR)
                # An unformatted (foreign) ZOS partition is a valid refusal fixture.
                if stream.read(8) == b"ZOSFS001":
                    validate_volume(stream, path.name, first * SECTOR)


def outside_zos_sha256(path: Path) -> str:
    """Digest of every byte of the test stick that is not inside the ZOS partition."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        digest.update(stream.read(ZOS_FIRST * SECTOR))
        stream.seek((ZOS_LAST + 1) * SECTOR)
        digest.update(stream.read())
    return digest.hexdigest()


def format_disk(name: str = "data.img", *, force: bool = False) -> Path:
    path = data_path(name)
    BUILD.mkdir(exist_ok=True)
    if path.exists() and not force:
        raise FileExistsError(f"{path.name} already exists; explicit mkfs --force is required")
    header, body = metadata()
    with path.open("wb" if force else "xb") as stream:
        stream.truncate(SECTORS * SECTOR)
        stream.write(superblock())
        stream.seek(BANKS[0] * SECTOR)
        stream.write(header)
        stream.write(body)
        stream.flush()
        os.fsync(stream.fileno())
    validate_disk(path)
    return path


def ensure_disk(name: str = "data.img") -> Path:
    path = data_path(name)
    if not path.exists():
        return format_disk(name)
    validate_disk(path)
    return path
