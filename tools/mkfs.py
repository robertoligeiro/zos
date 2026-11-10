"""Format only dedicated build/data*.img volumes for the ZOS simple filesystem."""

from __future__ import annotations

import os
from pathlib import Path
import re
import struct
import zlib

from mkimage import BUILD, image_path

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


def validate_disk(path: Path) -> None:
    if path.stat().st_size != SECTORS * SECTOR:
        raise ValueError(f"{path.name}: expected a 16 MiB ZOS data volume; refusing to reformat")
    with path.open("rb") as stream:
        if stream.read(SECTOR) != superblock():
            raise ValueError(f"{path.name}: invalid filesystem geometry/checksum; refusing to reformat")
        valid = False
        for bank in BANKS:
            stream.seek(bank * SECTOR)
            header, body = stream.read(SECTOR), stream.read(BODY_SECTORS * SECTOR)
            if header[:8] == b"ZOSBANK1" and struct.unpack_from("<Q", header, 8)[0] > 0 \
                    and zlib.crc32(header[:508]) == struct.unpack_from("<I", header, 508)[0] \
                    and zlib.crc32(body) == struct.unpack_from("<I", header, 16)[0]:
                valid = True
        if not valid:
            raise ValueError(f"{path.name}: no valid metadata checkpoint; refusing to reformat")


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
