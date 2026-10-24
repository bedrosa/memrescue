#!/usr/bin/env python3
"""Generador determinista de imágenes sintéticas con tablas de particiones MBR y GPT.

Uso:
    python -I gen_partition_images.py <directorio_salida> [--manifests <dir>]

- Solo usa la biblioteca estándar. No requiere privilegios ni toca dispositivos.
- Misma versión del script => mismos bytes (sin fechas, sin aleatoriedad).
- Escribe cada imagen (<nombre>.img) y su manifiesto (<nombre>.manifest.toml) con la verdad esperada:
  particiones, anomalías, estado de cada copia GPT y volúmenes que debe encontrar la búsqueda de
  particiones perdidas. El esquema del manifiesto se describe en docs/PRUEBAS.md (§1.1).

Referencias de formato (solo especificaciones públicas):
- UEFI 2.10, cap. 5: MBR protector, cabecera y entradas GPT, selección de copia.
- Estructura clásica de MBR/EBR: tabla en 0x1BE, 4 entradas de 16 bytes, firma 0x55AA; en un EBR la
  entrada de datos es relativa al propio EBR y la de enlace al inicio de la partición extendida.
- Microsoft "FAT: General Overview of On-Disk Format" (FAT32: BPB, FSInfo, copia en el sector 6).
- Microsoft "exFAT file system specification" (región de arranque de 12 sectores, copia en el 12,
  suma de comprobación del sector 11).
- Estructura pública del sector de arranque NTFS (BPB, copia en el último sector del volumen).
- Apple, "Inside Macintosh: Devices" (Driver Descriptor Map con firma ER en el bloque 0 y entradas
  del Apple Partition Map con firma PM en los bloques 1 a N, enteros big-endian).
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

GENERATOR_VERSION = 3
SCHEMA_VERSION = 2
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
    apm_index: int = 0
    apm_type: str = ""
    apm_name: str = ""
    apm_status: int = 0
    mark: bool = True  # False: el primer sector no lleva marcador (mapa APM, entrada fuera del disco)

    @property
    def end_lba(self) -> int:
        return self.start_lba + self.sectors - 1


@dataclass
class Anomaly:
    """Anomalía que un analizador correcto debe informar. `lba` está en coordenadas de la imagen."""
    code: str
    lba: int
    target_lba: int | None = None
    value: int | None = None
    partitions: list[str] = field(default_factory=list)


@dataclass
class Volume:
    """Volumen que la búsqueda de particiones perdidas debe encontrar (coordenadas de la imagen)."""
    fs: str
    start_lba: int
    sectors: int
    backup_boot_lba: int
    primary_boot: str  # "valid" | "damaged"
    backup_boot: str  # "valid"
    serial: str
    suggested_mbr_type: int
    hidden_sectors: int = 0  # inicio declarado por el propio FS (coordenadas del disco)


@dataclass
class Image:
    name: str
    sector_size: int
    total_sectors: int
    scheme: str
    scenario: str = ""
    partitions: list[Partition] = field(default_factory=list)
    disk_signature: int = 0
    disk_guid: str = ""
    data: bytearray = field(default_factory=bytearray)
    extra: dict = field(default_factory=dict)
    gpt_status: dict = field(default_factory=dict)
    hybrid_entries: list[dict] = field(default_factory=list)
    anomalies: list[Anomaly] = field(default_factory=list)
    volumes: list[Volume] = field(default_factory=list)
    leading_missing: int = 0
    trailing_missing: int = 0

    def put(self, lba: int, payload: bytes, offset: int = 0) -> None:
        pos = lba * self.sector_size + offset
        assert 0 <= pos and pos + len(payload) <= len(self.data), "escritura fuera de la imagen"
        self.data[pos : pos + len(payload)] = payload

    def mark_partitions(self) -> None:
        for p in self.partitions:
            if p.mark:
                self.put(p.start_lba, marker(self.name, p.label, p.start_lba))


def new_image(name: str, ss: int, total: int, scheme: str, scenario: str, **kw) -> Image:
    img = Image(name, ss, total, scheme, scenario=scenario, **kw)
    img.data = bytearray(ss * total)
    return img


# --------------------------------------------------------------------------------------------- MBR


def mbr_entry(active: bool, ptype: int, start: int, count: int) -> bytes:
    # CHS a 0xFEFFFF (uso de LBA), práctica habitual de los particionadores modernos.
    chs = b"\xfe\xff\xff"
    return struct.pack("<B3sB3sII", 0x80 if active else 0, chs, ptype, chs, start, count)


def write_mbr(img: Image, entries: list[bytes], lba: int = 0, disk_sig: int | None = None,
              signature: bool = True) -> None:
    sector = bytearray(img.sector_size)
    if disk_sig is not None:
        struct.pack_into("<I", sector, 0x1B8, disk_sig)
    for i, e in enumerate(entries):
        sector[0x1BE + 16 * i : 0x1BE + 16 * (i + 1)] = e
    if signature:
        sector[0x1FE:0x200] = b"\x55\xaa"
    img.put(lba, bytes(sector))


def primary(p: Partition) -> bytes:
    return mbr_entry(p.active, p.mbr_type, p.start_lba, p.sectors)


def write_ebr(img: Image, ebr: int, ext_start: int, data: Partition,
              link_lba: int | None = None, link_size: int = 0) -> None:
    """EBR: entrada 1 relativa al propio EBR; entrada 2 (enlace) relativa al inicio de la extendida."""
    entries = [mbr_entry(False, data.mbr_type, data.start_lba - ebr, data.sectors)]
    if link_lba is not None:
        entries.append(mbr_entry(False, 0x05, link_lba - ext_start, link_size))
    write_mbr(img, entries, lba=ebr)


def build_mbr_basic() -> Image:
    ss, total = 512, 32768  # 16 MiB
    img = new_image("mbr_basic", ss, total, "mbr",
                    "MBR sano: 2 primarias (una activa) y extendida 0x0F con cadena de 2 EBR",
                    disk_signature=0x4D520001)
    p1 = Partition("P1", 2048, 8192, mbr_type=0x07, active=True)
    p2 = Partition("P2", 10240, 4096, mbr_type=0x0C)
    ext_start, ext_size = 14336, total - 14336
    ebr1, ebr2 = ext_start, ext_start + 6144
    l1 = Partition("L1", ebr1 + 2048, 4096, mbr_type=0x83, logical=True)
    l2 = Partition("L2", ebr2 + 2048, 8192, mbr_type=0x07, logical=True)
    write_mbr(img, [primary(p1), primary(p2), mbr_entry(False, 0x0F, ext_start, ext_size)],
              disk_sig=img.disk_signature)
    write_ebr(img, ebr1, ext_start, l1, ebr2, total - ebr2)
    write_ebr(img, ebr2, ext_start, l2)
    img.partitions = [p1, p2, l1, l2]
    img.extra = {"extended_start_lba": ext_start, "extended_sectors": ext_size, "ebr_lbas": [ebr1, ebr2]}
    img.mark_partitions()
    return img


SMALL = 4096  # 2 MiB con sectores de 512 B: tamaño de los escenarios de tabla


def mbr_two_primaries(name: str, scenario: str, sig: int, p2: Partition, p1_active: bool = True,
                      signature: bool = True) -> Image:
    img = new_image(name, 512, SMALL, "mbr", scenario, disk_signature=sig)
    p1 = Partition("P1", 2048, 1024, mbr_type=0x07, active=p1_active)
    write_mbr(img, [primary(p1), primary(p2)], disk_sig=sig, signature=signature)
    img.partitions = [p1, p2]
    img.mark_partitions()
    return img


def build_mbr_no_signature() -> Image:
    img = mbr_two_primaries("mbr_no_signature", "MBR con entradas válidas pero sin la firma 0x55AA",
                            0x4D520101, Partition("P2", 3072, 1024, mbr_type=0x0C), signature=False)
    img.anomalies = [Anomaly("mbr_signature_missing", 0)]
    return img


def build_mbr_two_active() -> Image:
    img = mbr_two_primaries("mbr_two_active", "MBR con dos entradas marcadas como activas (0x80)",
                            0x4D520102, Partition("P2", 3072, 1024, mbr_type=0x83, active=True))
    img.anomalies = [Anomaly("mbr_multiple_active", 0, partitions=["P1", "P2"])]
    return img


def build_mbr_overlap() -> Image:
    img = mbr_two_primaries("mbr_overlap", "MBR con dos entradas primarias solapadas",
                            0x4D520103, Partition("P2", 2560, 1024, mbr_type=0x0C), p1_active=False)
    img.anomalies = [Anomaly("mbr_overlap", 0, partitions=["P1", "P2"])]
    return img


def build_mbr_unknown_type() -> Image:
    # 0x7F: reservado para uso individual; no figura en las tablas de tipos conocidos.
    img = mbr_two_primaries("mbr_unknown_type", "MBR con una entrada de tipo desconocido (0x7F)",
                            0x4D520104, Partition("P2", 3072, 512, mbr_type=0x7F), p1_active=False)
    img.anomalies = [Anomaly("mbr_unknown_type", 0, value=0x7F, partitions=["P2"])]
    return img


def build_mbr_outside_disk() -> Image:
    img = mbr_two_primaries("mbr_outside_disk", "MBR con una entrada que empieza dentro y acaba fuera del disco",
                            0x4D520105, Partition("P2", 3072, 2048, mbr_type=0x83), p1_active=False)
    img.anomalies = [Anomaly("mbr_outside_disk", 0, partitions=["P2"])]
    return img


def build_mbr_ext_chain(name: str, scenario: str, sig: int, ext_type: int, loop: bool = False,
                        link_outside: bool = False) -> Image:
    """Primaria P1 + extendida [2560, 4096) con EBR en 2560 y (salvo enlace fuera) en 3072."""
    img = new_image(name, 512, SMALL, "mbr", scenario, disk_signature=sig)
    p1 = Partition("P1", 2048, 512, mbr_type=0x83)
    ext_start, ext_size = 2560, SMALL - 2560
    ebr1, ebr2 = ext_start, 3072
    l1 = Partition("L1", ebr1 + 64, 448, mbr_type=0x83, logical=True)
    l2 = Partition("L2", ebr2 + 64, 448, mbr_type=0x07, logical=True)
    write_mbr(img, [primary(p1), mbr_entry(False, ext_type, ext_start, ext_size)], disk_sig=sig)
    img.partitions = [p1, l1]
    img.extra = {"extended_start_lba": ext_start, "extended_sectors": ext_size, "extended_type": ext_type}
    if link_outside:
        target = SMALL + 1000
        write_ebr(img, ebr1, ext_start, l1, target, 512)
        img.extra["ebr_lbas"] = [ebr1]
        img.anomalies = [Anomaly("ebr_link_outside_disk", ebr1, target_lba=target)]
    else:
        write_ebr(img, ebr1, ext_start, l1, ebr2, SMALL - ebr2)
        if loop:
            # El enlace del segundo EBR vuelve al primero (desplazamiento 0 desde la extendida).
            write_ebr(img, ebr2, ext_start, l2, ebr1, ext_size)
            img.anomalies = [Anomaly("ebr_loop", ebr2, target_lba=ebr1)]
        else:
            write_ebr(img, ebr2, ext_start, l2)
        img.partitions.append(l2)
        img.extra["ebr_lbas"] = [ebr1, ebr2]
    img.mark_partitions()
    return img


def build_empty_disk() -> Image:
    return new_image("empty_disk", 512, 2048, "none", "Disco sin tabla de particiones (todo a cero)",
                     anomalies=[Anomaly("mbr_missing", 0)])


# --------------------------------------------------------------------------------------------- GPT


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


def intact_gpt_status(total: int) -> dict:
    return {"gpt_primary_header": "valid", "gpt_primary_header_image_lba": 1,
            "gpt_primary_entries": "valid", "gpt_backup_header": "valid",
            "gpt_backup_header_image_lba": total - 1, "gpt_backup_entries": "valid",
            "gpt_selected": "primary"}


def build_gpt(name: str, ss: int, total: int, parts: list[Partition], scenario: str) -> Image:
    img = new_image(name, ss, total, "gpt", scenario)
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
    img.gpt_status = intact_gpt_status(total)
    img.mark_partitions()
    return img


BASIC_DATA = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"
LINUX_FS = "0fc63daf-8483-4772-8e79-3d69d8477de4"
EFI_SYSTEM = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"


def small_gpt_parts() -> list[Partition]:
    return [Partition("D1", 2048, 1024, gpt_type=BASIC_DATA, gpt_name="MR-DATA"),
            Partition("L1", 3072, 512, gpt_type=LINUX_FS, gpt_name="MR-LINUX")]


def corrupt_header(img: Image, lba: int) -> None:
    """Invierte LastUsableLBA sin recalcular la CRC: la firma sigue, la CRC32 de la cabecera falla."""
    pos = lba * img.sector_size + 48
    img.data[pos : pos + 8] = bytes(b ^ 0xFF for b in img.data[pos : pos + 8])


def build_gpt_damaged(name: str, scenario: str, ss: int = 512, total: int = SMALL,
                      parts: list[Partition] | None = None, primary_header: bool = False,
                      primary_entries: bool = False, backup_header: str = "") -> Image:
    img = build_gpt(name, ss, total, parts or small_gpt_parts(), scenario)
    st, an = img.gpt_status, []
    backup_lba = total - 1
    if primary_header:
        corrupt_header(img, 1)
        st.update(gpt_primary_header="crc", gpt_primary_entries="unchecked")
        an.append(Anomaly("gpt_primary_header_crc", 1))
    if primary_entries:
        # LBA final de D1 en la tabla primaria (sin recalcular la CRC de las entradas).
        img.put(2, struct.pack("<Q", 4000), offset=40)
        st.update(gpt_primary_entries="crc")
        an.append(Anomaly("gpt_primary_entries_crc", 2))
    if backup_header == "crc":
        corrupt_header(img, backup_lba)
        st.update(gpt_backup_header="crc", gpt_backup_entries="unchecked")
        an.append(Anomaly("gpt_backup_header_crc", backup_lba))
    elif backup_header == "missing":
        img.put(backup_lba, bytes(ss))
        img.put(img.extra["backup_entries_lba"], bytes(128 * 128))
        del st["gpt_backup_header_image_lba"]
        st.update(gpt_backup_header="missing", gpt_backup_entries="unchecked")
        an.append(Anomaly("gpt_backup_header_missing", backup_lba))
    prim_ok = st["gpt_primary_header"] == "valid" and st["gpt_primary_entries"] == "valid"
    back_ok = st["gpt_backup_header"] == "valid" and st["gpt_backup_entries"] == "valid"
    st["gpt_selected"] = "primary" if prim_ok else "backup" if back_ok else "none"
    if st["gpt_selected"] == "none":
        img.partitions = []
    img.anomalies = an
    return img


def build_protective_only() -> Image:
    img = new_image("protective_mbr_only", 512, 2048, "gpt",
                    "MBR protector (0xEE) sin cabeceras GPT en LBA 1 ni en el último sector")
    write_mbr(img, [mbr_entry(False, 0xEE, 1, 2047)])
    img.gpt_status = {"gpt_primary_header": "missing", "gpt_primary_entries": "unchecked",
                      "gpt_backup_header": "missing", "gpt_backup_entries": "unchecked",
                      "gpt_selected": "none"}
    img.anomalies = [Anomaly("gpt_primary_header_missing", 1), Anomaly("gpt_backup_header_missing", 2047)]
    return img


def build_hybrid() -> Image:
    parts = small_gpt_parts()
    img = build_gpt("hybrid_mbr_gpt", 512, SMALL, parts,
                    "GPT sana con MBR híbrido: 0xEE hasta la primera partición y dos entradas espejo")
    img.scheme = "hybrid"
    img.disk_signature = 0x4D520201
    d1, l1 = parts
    entries = [(0xEE, False, 1, d1.start_lba - 1, ""), (0x07, True, d1.start_lba, d1.sectors, "D1"),
               (0x83, False, l1.start_lba, l1.sectors, "L1")]
    write_mbr(img, [mbr_entry(a, t, s, c) for t, a, s, c, _ in entries], disk_sig=img.disk_signature)
    img.hybrid_entries = [{"index": i, "mbr_type": t, "active": a, "start_lba": s, "sectors": c,
                           "gpt_label": lab} for i, (t, a, s, c, lab) in enumerate(entries)]
    img.anomalies = [Anomaly("hybrid_mbr", 0)]
    return img


def build_gpt_cut(name: str, scenario: str, leading: int = 0, trailing: int = 0) -> Image:
    """GPT sana de SMALL sectores a la que se quitan sectores del principio o del final."""
    img = build_gpt(name, 512, SMALL, small_gpt_parts(), scenario)
    ss = img.sector_size
    img.data = img.data[leading * ss : len(img.data) - trailing * ss]
    img.total_sectors = n = SMALL - leading - trailing
    img.leading_missing, img.trailing_missing = leading, trailing
    st = img.gpt_status
    an = []
    if leading:
        an.append(Anomaly("mbr_missing", 0))
        an.append(Anomaly("image_shifted", -leading, value=leading))
        st["gpt_backup_header_image_lba"] = n - 1
        if leading > 1:
            del st["gpt_primary_header_image_lba"]
            st.update(gpt_primary_header="outside_image", gpt_primary_entries="unchecked",
                      gpt_selected="backup")
            an.append(Anomaly("gpt_primary_header_outside_image", 1 - leading))
        else:
            st["gpt_primary_header_image_lba"] = 1 - leading
        if img.extra["first_usable_lba"] - leading < 0:
            an.append(Anomaly("gpt_usable_range_outside_image", n - 1))
    if trailing:
        del st["gpt_backup_header_image_lba"]
        st.update(gpt_backup_header="outside_image", gpt_backup_entries="unchecked")
        an.append(Anomaly("protective_mbr_size_mismatch", 0, value=SMALL - 1))
        an.append(Anomaly("gpt_backup_header_outside_image", SMALL - 1))
        if img.extra["last_usable_lba"] > n - 1:
            an.append(Anomaly("gpt_usable_range_outside_image", 1))
    img.anomalies = an
    return img


# ------------------------------------------------------------------------- sistemas de archivos
#
# Sectores de arranque sintéticos según la especificación pública de cada sistema de archivos. Solo se
# escriben las estructuras que la búsqueda de particiones perdidas consulta (arranque, copia, FSInfo,
# región de arranque exFAT y cabecera de las FAT); no hay árbol de directorios ni MFT.


def ntfs_boot(ss: int, hidden: int, sectors: int, serial: int) -> bytes:
    spc = 8 if ss == 512 else 1
    clusters = sectors // spc
    s = bytearray(ss)
    s[0:3] = b"\xeb\x52\x90"
    s[3:11] = b"NTFS    "
    struct.pack_into("<HBH", s, 0x0B, ss, spc, 0)  # bytes/sector, sectores/clúster, reservados
    s[0x15] = 0xF8  # medio: disco fijo
    struct.pack_into("<HHI", s, 0x18, 63, 255, hidden)  # geometría y sectores ocultos (= inicio)
    struct.pack_into("<I", s, 0x24, 0x00800080)
    # Sectores totales = longitud del volumen - 1: la copia ocupa el último sector.
    struct.pack_into("<QQQ", s, 0x28, sectors - 1, 4, clusters // 2)  # total, LCN de $MFT y $MFTMirr
    struct.pack_into("<b", s, 0x40, -10)  # registro MFT de 2^10 = 1024 bytes
    struct.pack_into("<b", s, 0x44, 1)  # registro de índice de 1 clúster
    struct.pack_into("<Q", s, 0x48, serial)
    s[0x1FE:0x200] = b"\x55\xaa"
    return bytes(s)


def fat32_fat_size(tot: int, rsvd: int, spc: int, nfats: int) -> int:
    """Cálculo de FATSz32 de la especificación de Microsoft (FAT32)."""
    tmp1 = tot - rsvd
    tmp2 = ((256 * spc) + nfats) // 2
    return (tmp1 + tmp2 - 1) // tmp2


def exfat_checksum(region: bytes) -> int:
    """Suma del sector 11 (especificación exFAT): rotación a la derecha y suma, sin 106, 107 y 112."""
    c = 0
    for i, byte in enumerate(region):
        if i in (106, 107, 112):
            continue
        c = (((c & 1) << 31) | (c >> 1)) + byte
        c &= 0xFFFFFFFF
    return c


FAT32_MIN = 67584  # 33 MiB: el mínimo práctico con 65 525 clústeres de 512 B (especificación FAT)


class Disk:
    """Escribe volúmenes en coordenadas del disco original; la imagen puede estar desplazada."""

    def __init__(self, img: Image, shift: int = 0) -> None:
        self.img, self.shift = img, shift  # LBA de la imagen = LBA del disco - shift

    def put(self, disk_lba: int, payload: bytes) -> None:
        ss = self.img.sector_size
        first = disk_lba - self.shift
        for i in range(0, len(payload), ss):  # se descartan los sectores que caen fuera de la imagen
            if 0 <= first + i // ss < self.img.total_sectors:
                self.img.put(first + i // ss, payload[i : i + ss])

    def volume(self, fs: str, start: int, sectors: int, backup: int, serial: str, mbr_type: int,
               primary: str, backup_state: str) -> Volume:
        v = Volume(fs, start - self.shift, sectors, backup - self.shift, primary, backup_state, serial,
                   mbr_type, hidden_sectors=start)
        if v.start_lba < 0:
            v.primary_boot = "outside_image"
        self.img.volumes.append(v)
        return v

    def ntfs(self, start: int, sectors: int, serial: int, primary: str = "valid", backup: str = "valid") -> None:
        ss = self.img.sector_size
        boot = ntfs_boot(ss, start, sectors, serial)
        self.put(start, damaged_copy(boot, primary, b"\xeb\x52\x90NTFS    "))
        self.put(start + sectors - 1, damaged_copy(boot, backup, b"\xeb\x52\x90NTFS    "))
        self.volume("ntfs", start, sectors, start + sectors - 1, f"{serial:016x}", 0x07, primary, backup)

    def fat32(self, start: int, sectors: int, serial: int, primary: str = "valid") -> None:
        rsvd, spc, nfats = 32, 1, 2
        assert self.img.sector_size == 512
        fatsz = fat32_fat_size(sectors, rsvd, spc, nfats)
        clusters = (sectors - rsvd - nfats * fatsz) // spc
        assert clusters >= 65525, "FAT32 exige al menos 65 525 clústeres"
        b = bytearray(512)
        b[0:3] = b"\xeb\x58\x90"
        b[3:11] = b"MSWIN4.1"
        struct.pack_into("<HBHBHHBHHHII", b, 0x0B, 512, spc, rsvd, nfats, 0, 0, 0xF8, 0, 63, 255, start, sectors)
        struct.pack_into("<IHHIHH", b, 0x24, fatsz, 0, 0, 2, 1, 6)  # FATSz32, flags, versión, raíz, FSInfo, copia
        struct.pack_into("<BBBI", b, 0x40, 0x80, 0, 0x29, serial)
        b[0x47:0x52] = b"MR-FAT32   "
        b[0x52:0x5A] = b"FAT32   "
        b[0x1FE:0x200] = b"\x55\xaa"
        fsinfo = bytearray(512)
        struct.pack_into("<I", fsinfo, 0, 0x41615252)
        struct.pack_into("<III", fsinfo, 484, 0x61417272, clusters - 1, 3)
        struct.pack_into("<I", fsinfo, 508, 0xAA550000)
        self.put(start, damaged_copy(bytes(b), primary, b"\xf6"))
        self.put(start + 1, bytes(fsinfo))
        self.put(start + 6, bytes(b))
        self.put(start + 7, bytes(fsinfo))
        for i in range(nfats):
            self.put(start + rsvd + i * fatsz, struct.pack("<III", 0x0FFFFFF8, 0x0FFFFFFF, 0x0FFFFFFF).ljust(512, b"\0"))
        # Raíz en el clúster 2 con la entrada de etiqueta de volumen.
        self.put(start + rsvd + nfats * fatsz, (b"MR-FAT32   " + bytes([0x08])).ljust(512, b"\0"))
        self.volume("fat32", start, sectors, start + 6, f"{serial:08x}", 0x0C, primary, "valid")

    def exfat(self, start: int, sectors: int, serial: int, primary: str = "valid") -> None:
        ss = self.img.sector_size
        bps_shift = ss.bit_length() - 1
        spc_shift, fat_off, fat_len, heap_off = (3, 32, 8, 64) if ss == 512 else (0, 32, 8, 64)
        clusters = (sectors - heap_off) >> spc_shift
        assert sectors >= (1 << 20) >> bps_shift and (clusters + 2) * 4 <= fat_len * ss
        b = bytearray(ss)
        b[0:3] = b"\xeb\x76\x90"
        b[3:11] = b"EXFAT   "
        struct.pack_into("<QQIIIIIIHHBBBBB", b, 64, start, sectors, fat_off, fat_len, heap_off, clusters,
                         4, serial, 0x0100, 0, bps_shift, spc_shift, 1, 0x80, 0)
        b[510:512] = b"\x55\xaa"
        region = bytearray(ss * 11)
        region[0:ss] = b
        for i in range(1, 9):  # sectores de arranque extendidos: firma 0xAA550000 al final
            struct.pack_into("<I", region, (i + 1) * ss - 4, 0xAA550000)
        full = bytes(region) + struct.pack("<I", exfat_checksum(bytes(region))) * (ss // 4)
        self.put(start, damaged_copy(full[:ss], primary, b"\0") + full[ss:])
        self.put(start + 12, full)
        self.put(start + fat_off, struct.pack("<5I", 0xFFFFFFF8, *[0xFFFFFFFF] * 4).ljust(ss, b"\0"))
        self.volume("exfat", start, sectors, start + 12, f"{serial:08x}", 0x07, primary, "valid")


def damaged_copy(boot: bytes, state: str, filler: bytes) -> bytes:
    """Copia sana, dañada (relleno) o ilegible (relleno que imita la firma del FS y 0x55AA)."""
    if state == "valid":
        return boot
    if state == "unreadable":
        # Imita el contenido de un sector ilegible rellenado: firma del FS y 0x55AA, BPB sin sentido.
        pat = (b"\xeb\x52\x90NTFS    " * (len(boot) // 11 + 1))[: len(boot)]
        return pat[:0x1FE] + b"\x55\xaa" + pat[0x200:]
    return (filler * len(boot))[: len(boot)]


def lost_image(name: str, scenario: str, total: int, ss: int = 512, mbr: str = "zeroed",
               anomalies: list[Anomaly] | None = None) -> tuple[Image, Disk]:
    """Imagen sin tabla útil en LBA 0: `zeroed` (todo a cero) o `entries` (entradas a cero, firma intacta)."""
    img = new_image(name, ss, total, "none", scenario,
                    anomalies=anomalies if anomalies is not None else [Anomaly("mbr_missing", 0)])
    if mbr == "entries":
        write_mbr(img, [], disk_sig=0x4D520301)
        img.anomalies = [Anomaly("mbr_empty", 0)]
    return img, Disk(img)


def build_lost_volumes() -> list[Image]:
    out = []
    img, d = lost_image("lost_ntfs_mbr_zeroed", "MBR a cero; NTFS en LBA 2048 con copia en el último sector", 4096)
    d.ntfs(2048, 2048, 0x4D5253000000_1001)
    out.append(img)

    img, d = lost_image("lost_fat32_chs", "Entradas del MBR a cero con firma intacta; FAT32 en LBA 63 con copia en el sector 6",
                        63 + FAT32_MIN, mbr="entries")
    d.fat32(63, FAT32_MIN, 0x4D523002)
    out.append(img)

    img, d = lost_image("lost_exfat_1mib", "MBR a cero; exFAT en 1 MiB con regiones de arranque principal y de respaldo", 4096)
    d.exfat(2048, 2048, 0x4D523003)
    out.append(img)

    # NTFS, FAT32 y exFAT consecutivos; el exFAT empieza en una LBA no alineada a 1 MiB (por encadenamiento).
    fat_len = FAT32_MIN + 7
    third = 4096 + fat_len
    img, d = lost_image("lost_chain_three", "MBR a cero; NTFS, FAT32 y exFAT consecutivos, el tercero sin alinear",
                        third + 2048)
    d.ntfs(2048, 2048, 0x4D5253000000_1004)
    d.fat32(4096, fat_len, 0x4D523004)
    d.exfat(third, 2048, 0x4D523005)
    out.append(img)

    img, d = lost_image("ntfs_primary_damaged", "MBR a cero; arranque NTFS principal a cero y copia final intacta", 4096)
    d.ntfs(2048, 2048, 0x4D5253000000_1006, primary="damaged")
    out.append(img)

    img, d = lost_image("fat32_primary_damaged", "MBR a cero; arranque FAT32 principal dañado y copia del sector 6 intacta",
                        2048 + FAT32_MIN)
    d.fat32(2048, FAT32_MIN, 0x4D523007, primary="damaged")
    out.append(img)

    img, d = lost_image("exfat_primary_damaged", "MBR a cero; sector de arranque exFAT principal a cero y región de respaldo intacta", 4096)
    d.exfat(2048, 2048, 0x4D523008, primary="damaged")
    out.append(img)

    img, d = lost_image("primary_unreadable", "NTFS cuyo arranque principal se debe simular ilegible; contiene un relleno que imita NTFS y 0x55AA", 4096)
    d.ntfs(2048, 2048, 0x4D5253000000_1009, primary="unreadable")
    img.extra["unreadable_lbas"] = [2048]
    out.append(img)

    img, d = lost_image("backup_unreadable", "NTFS cuya copia del arranque se debe simular ilegible; contiene un relleno que imita NTFS y 0x55AA", 4096)
    d.ntfs(2048, 2048, 0x4D5253000000_100A, backup="unreadable")
    img.extra["unreadable_lbas"] = [4095]
    out.append(img)

    # Disco original de 6144 sectores con MBR (NTFS en 2048 y exFAT en 4096) visto con k sectores de más o de menos.
    for name, k, scenario in (("image_shifted_pos", -8, "Imagen con 8 sectores a cero añadidos al inicio (MBR en LBA 8)"),
                              ("image_shifted_neg", 2100, "Imagen a la que faltan los 2100 primeros sectores: MBR y arranque NTFS perdidos")):
        img = new_image(name, 512, 6144 - k, "none", scenario)
        img.leading_missing = k
        d = Disk(img, shift=k)
        sector = bytearray(512)
        struct.pack_into("<I", sector, 0x1B8, 0x4D520302)
        sector[0x1BE:0x1DE] = mbr_entry(False, 0x07, 2048, 2048) + mbr_entry(False, 0x07, 4096, 2048)
        sector[0x1FE:0x200] = b"\x55\xaa"
        d.put(0, bytes(sector))
        d.ntfs(2048, 2048, 0x4D5253000000_100B)
        d.exfat(4096, 2048, 0x4D52300C)
        img.anomalies = [Anomaly("mbr_missing", 0), Anomaly("image_shifted", -k, value=k)]
        if k < 0:
            img.anomalies.append(Anomaly("mbr_displaced", -k))
        out.append(img)

    img, d = lost_image("gpt_4kn_lost", "Disco 4Kn con GPT y MBR protector borrados; NTFS y exFAT de 4096 B/sector", 1024, ss=4096)
    d.ntfs(256, 256, 0x4D5253000000_100D)
    d.exfat(512, 256, 0x4D52300E)
    out.append(img)

    img, d = lost_image("superfloppy", "Sin tabla: volumen NTFS que empieza en LBA 0", 2048,
                        anomalies=[Anomaly("superfloppy", 0)])
    d.ntfs(0, 2048, 0x4D5253000000_100F)
    out.append(img)

    img = new_image("random_disk", 512, 2048, "none", "Disco de bytes pseudoaleatorios deterministas (sin tabla ni volúmenes)",
                    anomalies=[Anomaly("mbr_missing", 0)])
    stream = b"".join(hashlib.sha256(b"memrescue-random/%d" % i).digest() for i in range(len(img.data) // 32))
    img.data[:] = stream
    out.append(img)

    img = new_image("fake_mbr_in_data", 512, 4096, "mbr", "MBR sano con una partición cuyo contenido incluye un sector con aspecto de MBR",
                    disk_signature=0x4D520303)
    p1 = Partition("P1", 2048, 2048, mbr_type=0x07)
    write_mbr(img, [primary(p1)], disk_sig=img.disk_signature)
    img.partitions = [p1]
    img.mark_partitions()
    decoy = 2048 + 64
    write_mbr(img, [mbr_entry(True, 0x07, 100, 500), mbr_entry(False, 0x0C, 700, 300)], lba=decoy, disk_sig=0x4D52DEC0)
    img.extra["decoy_lbas"] = [decoy]
    out.append(img)
    return out


# --------------------------------------------------------------------------------------------- APM


APM_MAP = ("MAP", 1, 63, "Apple_partition_map", "Apple", 0x37)
APM_ENTRIES = [
    APM_MAP,
    ("H1", 64, 2048, "Apple_HFS", "MR-HFS", 0x40000033),
    ("F1", 2112, 1024, "Apple_Free", "Extra", 0),
    ("H2", 3136, 960, "Apple_HFS", "MR-DATA", 0x40000033),
]


def apm_entry(count: int, start: int, size: int, ptype: str, name: str, status: int) -> bytes:
    """Entrada del mapa (512 B, big-endian): firma PM, nº de entradas, inicio, tamaño, nombre, tipo,
    área de datos lógica (toda la partición) y estado."""
    e = bytearray(512)
    struct.pack_into(">2sHIII32s32sIII", e, 0, b"PM", 0, count, start, size,
                     name.encode("ascii"), ptype.encode("ascii"), 0, size, status)
    return bytes(e)


def build_apm(name: str, scenario: str, entries: list[tuple], damaged: tuple[int, ...] = ()) -> Image:
    """Disco APM de 4096 bloques de 512 B: DDM (ER, tamaño de bloque y nº de bloques) en el bloque 0 y
    una entrada por bloque desde el 1. Las entradas de `damaged` (desde 1) pierden la firma PM."""
    ss, total = 512, 4096
    img = new_image(name, ss, total, "apm", scenario)
    ddm = bytearray(ss)
    struct.pack_into(">2sHI", ddm, 0, b"ER", ss, total)
    img.put(0, bytes(ddm))
    img.anomalies = [Anomaly("mbr_missing", 0)]
    for i, (label, start, size, ptype, pname, status) in enumerate(entries, start=1):
        e = bytearray(apm_entry(len(entries), start, size, ptype, pname, status))
        if i in damaged:
            e[0:2] = b"\x00\x00"
            img.anomalies.append(Anomaly("apm_entry_signature", i, value=i))
            img.put(i, bytes(e))
            continue
        img.put(i, bytes(e))
        inside = start < total
        if not inside:
            img.anomalies.append(Anomaly("apm_entry_outside_disk", i, value=i))
        elif start + size > total:
            img.anomalies.append(Anomaly("apm_entry_partially_outside_disk", i, value=i))
        img.partitions.append(Partition(label, start, size, apm_index=i, apm_type=ptype, apm_name=pname,
                                        apm_status=status, mark=inside and ptype != "Apple_partition_map"))
    img.extra = {"apm_block_size": ss, "apm_disk_blocks": total, "apm_map_entries": len(entries)}
    img.mark_partitions()
    return img


def build_apm_all() -> list[Image]:
    outside = [APM_ENTRIES[0], APM_ENTRIES[1], ("F1", 2112, 2000, "Apple_Free", "Extra", 0),
               ("H2", 5000, 960, "Apple_HFS", "MR-DATA", 0x40000033)]
    return [
        build_apm("apm_basic", "Apple Partition Map sano: DDM de 512 B, mapa de 4 entradas (mapa, 2 Apple_HFS y Apple_Free)",
                  APM_ENTRIES),
        build_apm("apm_entry_damaged", "Apple Partition Map con la entrada 3 sin la firma PM (no se lista)",
                  APM_ENTRIES, damaged=(3,)),
        build_apm("apm_entry_outside", "Apple Partition Map con una entrada que termina fuera del disco y otra que empieza fuera",
                  outside),
    ]


# ------------------------------------------------------------------------------------- catálogo


def build_all() -> list[Image]:
    gpt_basic = build_gpt("gpt_basic", 512, 32768, [
        Partition("D1", 2048, 8192, gpt_type=BASIC_DATA, gpt_name="MR-DATA1"),
        Partition("L1", 10240, 12288, gpt_type=LINUX_FS, gpt_name="MR-LINUX"),
        Partition("E1", 22528, 2048, gpt_type=EFI_SYSTEM, gpt_name="MR-ESP"),
    ], "GPT sana: MBR protector, cabecera y entradas primarias y de respaldo, 3 particiones")
    gpt_4kn = build_gpt("gpt_4kn", 4096, 2048, [
        Partition("D1", 256, 1024, gpt_type=BASIC_DATA, gpt_name="MR-4KN"),
    ], "GPT sana con sectores de 4096 B")
    return [
        build_mbr_basic(),
        gpt_basic,
        gpt_4kn,
        # MBR/EBR dañados o límite
        build_mbr_no_signature(),
        build_mbr_two_active(),
        build_mbr_ext_chain("mbr_ext_0x85", "Extendida de tipo 0x85 (Linux) con cadena de 2 EBR",
                            0x4D520106, 0x85),
        build_mbr_ext_chain("mbr_ebr_loop", "Cadena de EBR en bucle: el enlace del segundo EBR vuelve al primero",
                            0x4D520107, 0x0F, loop=True),
        build_mbr_ext_chain("mbr_ebr_link_outside", "Enlace de EBR que apunta fuera del disco",
                            0x4D520108, 0x0F, link_outside=True),
        build_mbr_overlap(),
        build_mbr_unknown_type(),
        build_mbr_outside_disk(),
        build_empty_disk(),
        build_protective_only(),
        build_hybrid(),
        # GPT dañadas, desplazadas o recortadas
        build_gpt_damaged("gpt_primary_header_bad", "Cabecera GPT primaria con CRC32 incorrecta; respaldo sano",
                          primary_header=True),
        build_gpt_damaged("gpt_entries_crc_bad", "Tabla de entradas primaria alterada (CRC32 incorrecta); respaldo sano",
                          primary_entries=True),
        build_gpt_damaged("gpt_backup_header_bad", "Cabecera GPT de respaldo con CRC32 incorrecta; primaria sana",
                          backup_header="crc"),
        build_gpt_damaged("gpt_backup_missing", "Cabecera y entradas GPT de respaldo borradas; primaria sana",
                          backup_header="missing"),
        build_gpt_damaged("gpt_both_bad", "Cabecera primaria con CRC32 incorrecta y respaldo borrado",
                          primary_header=True, backup_header="missing"),
        build_gpt_damaged("gpt_4kn_primary_bad", "GPT 4Kn con la cabecera primaria dañada; respaldo sano",
                          ss=4096, total=512, primary_header=True,
                          parts=[Partition("D1", 256, 128, gpt_type=BASIC_DATA, gpt_name="MR-4KN")]),
        build_gpt_cut("gpt_shifted_start", "Imagen GPT a la que faltan los 63 primeros sectores (LBA desplazadas)",
                      leading=63),
        build_gpt_cut("gpt_shifted_start_1", "Imagen GPT a la que falta el primer sector (cabecera primaria en LBA 0)",
                      leading=1),
        build_gpt_cut("gpt_truncated_end", "Imagen GPT recortada al final: faltan 200 sectores y el respaldo",
                      trailing=200),
        # Apple Partition Map (#17)
        *build_apm_all(),
        # Volúmenes perdidos (tabla borrada) y casos negativos de la búsqueda (INV-05b §9)
        *build_lost_volumes(),
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
        "# Esquema en docs/PRUEBAS.md (§1.1). Las LBA de [[partitions]], de la GPT y hidden_sectors son las",
        "# declaradas (disco original); las demás, de la imagen: LBA imagen = LBA disco - leading_sectors_missing.",
        f"schema_version = {SCHEMA_VERSION}",
        f"generator_version = {GENERATOR_VERSION}",
        f'name = "{img.name}"',
        f'file = "{img.name}.img"',
        f"scenario = {toml_value(img.scenario)}",
        f'scheme = "{img.scheme}"',
        f"sector_size = {img.sector_size}",
        f"total_sectors = {img.total_sectors}",
        f"size_bytes = {len(img.data)}",
        f'sha256 = "{sha256}"',
        f"leading_sectors_missing = {img.leading_missing}",
        f"trailing_sectors_missing = {img.trailing_missing}",
    ]
    if img.scheme in ("mbr", "hybrid"):
        lines.append(f'disk_signature = "{img.disk_signature:08x}"')
    if img.disk_guid:
        lines.append(f'disk_guid = "{img.disk_guid}"')
    for k, v in img.extra.items():
        lines.append(f"{k} = {toml_value(v)}")
    for k, v in img.gpt_status.items():
        lines.append(f"{k} = {toml_value(v)}")
    gpt_keys = img.scheme in ("gpt", "hybrid")
    for p in img.partitions:
        lines += ["", "[[partitions]]", f'label = "{p.label}"', f"start_lba = {p.start_lba}",
                  f"sectors = {p.sectors}", f"end_lba = {p.end_lba}"]
        if img.scheme == "apm":
            lines += [f"apm_index = {p.apm_index}", f"apm_type = {toml_value(p.apm_type)}",
                      f"apm_name = {toml_value(p.apm_name)}", f"apm_status = {p.apm_status}"]
        elif not gpt_keys:
            lines += [f"mbr_type = {p.mbr_type}", f"active = {toml_value(p.active)}",
                      f"logical = {toml_value(p.logical)}"]
        else:
            lines += [f'gpt_type = "{p.gpt_type}"', f'gpt_name = "{p.gpt_name}"',
                      f'unique_guid = "{det_guid(f"{img.name}/{p.label}")}"']
        if img.leading_missing:
            lines.append(f"marker_lba = {p.start_lba - img.leading_missing}")
        text = marker(img.name, p.label, p.start_lba).decode() if p.mark else ""
        lines.append(f'marker = "{text}"')
    for h in img.hybrid_entries:
        lines += ["", "[[hybrid_mbr_entries]]"] + [f"{k} = {toml_value(v)}" for k, v in h.items()]
    for a in img.anomalies:
        lines += ["", "[[anomalies]]", f'code = "{a.code}"', f"lba = {a.lba}"]
        if a.target_lba is not None:
            lines.append(f"target_lba = {a.target_lba}")
        if a.value is not None:
            lines.append(f"value = {a.value}")
        if a.partitions:
            lines.append(f"partitions = {toml_value(a.partitions)}")
    for v in img.volumes:
        evidence = [e for e, st in (("boot", v.primary_boot), ("backup_boot", v.backup_boot)) if st == "valid"]
        lines += ["", "[[found_volumes]]", f'fs = "{v.fs}"', f"start_lba = {v.start_lba}",
                  f"sectors = {v.sectors}", f"end_lba = {v.start_lba + v.sectors - 1}",
                  f"hidden_sectors = {v.hidden_sectors}",
                  f"boot_lba = {v.start_lba}", f"backup_boot_lba = {v.backup_boot_lba}",
                  f'primary_boot = "{v.primary_boot}"', f'backup_boot = "{v.backup_boot}"',
                  f"evidence = {toml_value(evidence)}", f"partial = {toml_value(v.start_lba < 0)}",
                  f'serial = "{v.serial}"', f"suggested_mbr_type = {v.suggested_mbr_type}"]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("out_dir", type=Path)
    ap.add_argument("--manifests", type=Path, help="directorio adicional donde copiar los manifiestos")
    a = ap.parse_args(argv)
    a.out_dir.mkdir(parents=True, exist_ok=True)
    for img in build_all():
        assert len(img.data) == img.sector_size * img.total_sectors, img.name
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
