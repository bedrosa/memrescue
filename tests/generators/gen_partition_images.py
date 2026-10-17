#!/usr/bin/env python3
"""Generador determinista de imágenes sintéticas con tablas de particiones MBR y GPT.

Uso:
    python -I gen_partition_images.py <directorio_salida> [--manifests <dir>]

- Solo usa la biblioteca estándar. No requiere privilegios ni toca dispositivos.
- Misma versión del script => mismos bytes (sin fechas, sin aleatoriedad).
- Escribe cada imagen (<nombre>.img) y su manifiesto (<nombre>.manifest.toml) con la verdad esperada.

Referencias de formato: especificación UEFI 2.10, cap. 5 (MBR protector, cabecera y entradas GPT);
estructura clásica de MBR/EBR (tabla en 0x1BE, 4 entradas de 16 bytes, firma 0x55AA).
"""
from __future__ import annotations

import argparse
import hashlib
import struct
import sys
import uuid
import zlib
from dataclasses import dataclass, field
from pathlib import Path

GENERATOR_VERSION = 1
NS = uuid.UUID("6d656d72-6573-6375-6500-000000000001")  # espacio de nombres fijo de MemRescue


def det_guid(name: str) -> uuid.UUID:
    """GUID determinista a partir de un nombre."""
    return uuid.uuid5(NS, name)


def marker(image: str, label: str, lba: int) -> bytes:
    """Contenido reconocible del primer sector de cada partición (para verificar el mapeo)."""
    return f"MEMRESCUE-TEST|{image}|{label}|LBA={lba}|".encode("ascii")


@dataclass
class Partition:
    label: str
    start_lba: int
    sectors: int
    mbr_type: int = 0
    gpt_type: str = ""
    gpt_name: str = ""
    active: bool = False
    logical: bool = False

    @property
    def end_lba(self) -> int:
        return self.start_lba + self.sectors - 1


@dataclass
class Image:
    name: str
    sector_size: int
    total_sectors: int
    scheme: str
    partitions: list[Partition] = field(default_factory=list)
    disk_signature: int = 0
    disk_guid: str = ""
    data: bytearray = field(default_factory=bytearray)
    extra: dict = field(default_factory=dict)

    def put(self, lba: int, payload: bytes, offset: int = 0) -> None:
        pos = lba * self.sector_size + offset
        assert pos + len(payload) <= len(self.data), "escritura fuera de la imagen"
        self.data[pos : pos + len(payload)] = payload


def mbr_entry(active: bool, ptype: int, start: int, count: int) -> bytes:
    # CHS a 0xFEFFFF (uso de LBA), práctica habitual de los particionadores modernos.
    chs = b"\xfe\xff\xff"
    return struct.pack("<B3sB3sII", 0x80 if active else 0, chs, ptype, chs, start, count)


def write_mbr(img: Image, entries: list[bytes], lba: int = 0, disk_sig: int | None = None) -> None:
    sector = bytearray(img.sector_size)
    if disk_sig is not None:
        struct.pack_into("<I", sector, 0x1B8, disk_sig)
    for i, e in enumerate(entries):
        sector[0x1BE + 16 * i : 0x1BE + 16 * (i + 1)] = e
    sector[0x1FE:0x200] = b"\x55\xaa"
    img.put(lba, bytes(sector))


def build_mbr_basic() -> Image:
    ss, total = 512, 32768  # 16 MiB
    img = Image("mbr_basic", ss, total, "mbr", disk_signature=0x4D520001)
    img.data = bytearray(ss * total)
    p1 = Partition("P1", 2048, 8192, mbr_type=0x07, active=True)
    p2 = Partition("P2", 10240, 4096, mbr_type=0x0C)
    ext_start, ext_size = 14336, total - 14336
    ebr1, ebr2 = ext_start, ext_start + 6144
    l1 = Partition("L1", ebr1 + 2048, 4096, mbr_type=0x83, logical=True)
    l2 = Partition("L2", ebr2 + 2048, 8192, mbr_type=0x07, logical=True)
    write_mbr(
        img,
        [
            mbr_entry(True, p1.mbr_type, p1.start_lba, p1.sectors),
            mbr_entry(False, p2.mbr_type, p2.start_lba, p2.sectors),
            mbr_entry(False, 0x0F, ext_start, ext_size),
        ],
        disk_sig=img.disk_signature,
    )
    # EBR: entrada 1 relativa al propio EBR; entrada 2 (enlace) relativa al inicio de la extendida.
    write_mbr(img, [mbr_entry(False, l1.mbr_type, l1.start_lba - ebr1, l1.sectors),
                    mbr_entry(False, 0x05, ebr2 - ext_start, total - ebr2)], lba=ebr1)
    write_mbr(img, [mbr_entry(False, l2.mbr_type, l2.start_lba - ebr2, l2.sectors)], lba=ebr2)
    img.partitions = [p1, p2, l1, l2]
    img.extra = {"extended_start_lba": ext_start, "extended_sectors": ext_size, "ebr_lbas": [ebr1, ebr2]}
    for p in img.partitions:
        img.put(p.start_lba, marker(img.name, p.label, p.start_lba))
    return img


def gpt_entry(p: Partition, unique: uuid.UUID) -> bytes:
    name = p.gpt_name.encode("utf-16-le").ljust(72, b"\x00")
    return (
        uuid.UUID(p.gpt_type).bytes_le
        + unique.bytes_le
        + struct.pack("<QQQ", p.start_lba, p.end_lba, 0)
        + name
    )


def gpt_header(my_lba: int, alt_lba: int, first: int, last: int, disk_guid: uuid.UUID,
               entries_lba: int, n_entries: int, entry_size: int, entries_crc: int,
               sector_size: int) -> bytes:
    hdr = bytearray(
        struct.pack(
            "<8sIIIIQQQQ16sQIII",
            b"EFI PART", 0x00010000, 92, 0, 0,
            my_lba, alt_lba, first, last,
            disk_guid.bytes_le, entries_lba, n_entries, entry_size, entries_crc,
        )
    )
    struct.pack_into("<I", hdr, 16, zlib.crc32(bytes(hdr)) & 0xFFFFFFFF)
    return bytes(hdr).ljust(sector_size, b"\x00")


def build_gpt(name: str, ss: int, total: int, parts: list[Partition]) -> Image:
    img = Image(name, ss, total, "gpt")
    img.data = bytearray(ss * total)
    n_entries, entry_size = 128, 128
    entry_sectors = (n_entries * entry_size + ss - 1) // ss
    first_usable = 2 + entry_sectors
    last_usable = total - 2 - entry_sectors
    disk_guid = det_guid(f"{name}/disk")
    img.disk_guid = str(disk_guid)

    table = bytearray(n_entries * entry_size)
    for i, p in enumerate(parts):
        assert first_usable <= p.start_lba <= p.end_lba <= last_usable, p
        table[i * entry_size : (i + 1) * entry_size] = gpt_entry(p, det_guid(f"{name}/{p.label}"))
    entries_crc = zlib.crc32(bytes(table)) & 0xFFFFFFFF

    # MBR protector (UEFI 5.2.3): una entrada tipo 0xEE desde LBA 1.
    write_mbr(img, [mbr_entry(False, 0xEE, 1, min(total - 1, 0xFFFFFFFF))])
    backup_entries_lba = total - 1 - entry_sectors
    img.put(1, gpt_header(1, total - 1, first_usable, last_usable, disk_guid, 2,
                          n_entries, entry_size, entries_crc, ss))
    img.put(2, bytes(table))
    img.put(backup_entries_lba, bytes(table))
    img.put(total - 1, gpt_header(total - 1, 1, first_usable, last_usable, disk_guid,
                                  backup_entries_lba, n_entries, entry_size, entries_crc, ss))
    img.partitions = parts
    img.extra = {
        "first_usable_lba": first_usable,
        "last_usable_lba": last_usable,
        "entries_lba": 2,
        "backup_entries_lba": backup_entries_lba,
        "backup_header_lba": total - 1,
        "entry_count": n_entries,
        "entry_size": entry_size,
        "entries_crc32": f"{entries_crc:08x}",
    }
    for p in parts:
        img.put(p.start_lba, marker(name, p.label, p.start_lba))
    return img


BASIC_DATA = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"
LINUX_FS = "0fc63daf-8483-4772-8e79-3d69d8477de4"
EFI_SYSTEM = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"


def build_all() -> list[Image]:
    return [
        build_mbr_basic(),
        build_gpt("gpt_basic", 512, 32768, [
            Partition("D1", 2048, 8192, gpt_type=BASIC_DATA, gpt_name="MR-DATA1"),
            Partition("L1", 10240, 12288, gpt_type=LINUX_FS, gpt_name="MR-LINUX"),
            Partition("E1", 22528, 2048, gpt_type=EFI_SYSTEM, gpt_name="MR-ESP"),
        ]),
        build_gpt("gpt_4kn", 4096, 2048, [
            Partition("D1", 256, 1024, gpt_type=BASIC_DATA, gpt_name="MR-4KN"),
        ]),
    ]


def toml_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    return '"' + str(v).replace("\\", "\\\\").replace('"', '\\"') + '"'


def manifest(img: Image, sha256: str) -> str:
    lines = [
        "# Generado por tests/generators/gen_partition_images.py: no editar a mano.",
        f"generator_version = {GENERATOR_VERSION}",
        f'name = "{img.name}"',
        f'file = "{img.name}.img"',
        f'scheme = "{img.scheme}"',
        f"sector_size = {img.sector_size}",
        f"total_sectors = {img.total_sectors}",
        f"size_bytes = {len(img.data)}",
        f'sha256 = "{sha256}"',
    ]
    if img.scheme == "mbr":
        lines.append(f'disk_signature = "{img.disk_signature:08x}"')
    if img.disk_guid:
        lines.append(f'disk_guid = "{img.disk_guid}"')
    for k, v in img.extra.items():
        lines.append(f"{k} = {toml_value(v)}")
    for p in img.partitions:
        lines += ["", "[[partitions]]", f'label = "{p.label}"', f"start_lba = {p.start_lba}",
                  f"sectors = {p.sectors}", f"end_lba = {p.end_lba}"]
        if img.scheme == "mbr":
            lines += [f"mbr_type = {p.mbr_type}", f"active = {toml_value(p.active)}",
                      f"logical = {toml_value(p.logical)}"]
        else:
            lines += [f'gpt_type = "{p.gpt_type}"', f'gpt_name = "{p.gpt_name}"',
                      f'unique_guid = "{det_guid(f"{img.name}/{p.label}")}"']
        lines.append(f'marker = "{marker(img.name, p.label, p.start_lba).decode()}"')
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("out_dir", type=Path)
    ap.add_argument("--manifests", type=Path, help="directorio adicional donde copiar los manifiestos")
    a = ap.parse_args(argv)
    a.out_dir.mkdir(parents=True, exist_ok=True)
    for img in build_all():
        sha = hashlib.sha256(img.data).hexdigest()
        (a.out_dir / f"{img.name}.img").write_bytes(img.data)
        text = manifest(img, sha)
        (a.out_dir / f"{img.name}.manifest.toml").write_text(text, encoding="utf-8", newline="\n")
        if a.manifests:
            a.manifests.mkdir(parents=True, exist_ok=True)
            (a.manifests / f"{img.name}.manifest.toml").write_text(text, encoding="utf-8", newline="\n")
        print(f"{img.name}.img  {len(img.data)} bytes  sha256={sha}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
