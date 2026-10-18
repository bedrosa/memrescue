#!/usr/bin/env python3
"""Validador independiente de las imágenes MBR/GPT del corpus.

Uso:
    python -I validate_partition_images.py <dir_imagenes> <dir_manifiestos_de_referencia>

No importa el generador: vuelve a interpretar cada imagen desde los bytes según la especificación
(MBR/EBR clásico, UEFI para GPT, Microsoft para FAT32 y exFAT, estructura pública del arranque NTFS),
deduce por su cuenta el esquema, las particiones, las anomalías, el estado de cada copia GPT, el
desplazamiento de la imagen y los volúmenes con sector de arranque, y lo compara con el manifiesto de
referencia versionado en git (tests/corpus/manifests). Comprueba también que la generación es
determinista (SHA-256 igual al de referencia).

Código de salida 0 si todo es correcto; 1 si hay discrepancias.
"""
from __future__ import annotations

import hashlib
import struct
import sys
import tomllib
import uuid
import zlib
from pathlib import Path

# Tipos de partición MBR con asignación pública conocida (listas públicas de identificadores).
KNOWN_MBR_TYPES = {
    0x01, 0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0E, 0x0F, 0x11, 0x12, 0x14, 0x16, 0x17, 0x1B, 0x1C,
    0x1E, 0x27, 0x42, 0x82, 0x83, 0x85, 0x8E, 0xA5, 0xA6, 0xA8, 0xA9, 0xAB, 0xAF, 0xEE, 0xEF, 0xFB,
    0xFD,
}
EXTENDED = (0x05, 0x0F, 0x85)
MAX_EBR = 256
SCHEMA_VERSION = 2


class Report:
    def __init__(self, name: str) -> None:
        self.name, self.errors, self.checks = name, [], 0

    def check(self, cond: bool, msg: str) -> bool:
        self.checks += 1
        if not cond:
            self.errors.append(msg)
        return cond


class Disk:
    def __init__(self, data: bytes, ss: int) -> None:
        self.data, self.ss, self.n = data, ss, len(data) // ss

    def sector(self, lba: int) -> bytes | None:
        if not 0 <= lba < self.n:
            return None
        return self.data[lba * self.ss : (lba + 1) * self.ss]

    def bytes_at(self, lba: int, length: int) -> bytes | None:
        if lba < 0 or lba * self.ss + length > len(self.data):
            return None
        return self.data[lba * self.ss : lba * self.ss + length]


# ------------------------------------------------------------------------------------- MBR / EBR


def mbr_entries(sec: bytes) -> list[dict]:
    out = []
    for i in range(4):
        e = sec[0x1BE + 16 * i : 0x1BE + 16 * (i + 1)]
        start, count = struct.unpack_from("<II", e, 8)
        out.append({"index": i, "boot": e[0], "type": e[4], "start": start, "count": count})
    return out


def has_sig(sec: bytes) -> bool:
    return sec[0x1FE:0x200] == b"\x55\xaa"


def looks_like_table(sec: bytes) -> bool:
    """Criterio de tabla: entradas usadas con indicador 0x00/0x80 y como mucho un enlace extendido."""
    used = [e for e in mbr_entries(sec) if e["type"]]
    return (bool(used) and all(e["boot"] in (0, 0x80) for e in used)
            and sum(e["type"] in (0x05, 0x0F) for e in used) <= 1)


def analyze_mbr(d: Disk, out: dict) -> str:
    """Clasifica LBA 0: missing, empty, superfloppy, protective, hybrid o mbr."""
    s0 = d.sector(0)
    an, parts = out["anomalies"], out["partitions"]
    if boot_kind(s0, d.ss):
        an.append({"code": "superfloppy", "lba": 0})
        return "superfloppy"
    entries = [e for e in mbr_entries(s0) if e["type"]]
    if not entries:
        an.append({"code": "mbr_empty" if has_sig(s0) else "mbr_missing", "lba": 0})
        return "empty" if has_sig(s0) else "missing"
    if not looks_like_table(s0):
        an.append({"code": "mbr_missing", "lba": 0})
        return "missing"
    out["disk_signature"] = f"{struct.unpack_from('<I', s0, 0x1B8)[0]:08x}"
    if any(e["type"] == 0xEE for e in entries):
        if len(entries) == 1:
            e = entries[0]
            out["protective"] = e
            if e["count"] != min(d.n - 1, 0xFFFFFFFF):
                an.append({"code": "protective_mbr_size_mismatch", "lba": 0, "value": e["count"]})
            return "protective"
        out["hybrid"] = entries
        an.append({"code": "hybrid_mbr", "lba": 0})
        return "hybrid"
    if not has_sig(s0):
        an.append({"code": "mbr_signature_missing", "lba": 0})
    active = [e for e in entries if e["boot"] == 0x80]
    ext = None
    for e in entries:
        if e["type"] in EXTENDED:
            ext = e
            continue
        parts.append({"start_lba": e["start"], "sectors": e["count"], "mbr_type": e["type"],
                      "active": e["boot"] == 0x80, "logical": False})
    if len(active) > 1:
        an.append({"code": "mbr_multiple_active", "lba": 0, "starts": sorted(e["start"] for e in active)})
    if ext:
        out["extended"] = (ext["start"], ext["count"], ext["type"])
        walk_ebr(d, ext["start"], ext["count"], out)
    for p in parts:
        if p["mbr_type"] not in KNOWN_MBR_TYPES:
            an.append({"code": "mbr_unknown_type", "lba": 0, "value": p["mbr_type"], "starts": [p["start_lba"]]})
        if p["start_lba"] + p["sectors"] > d.n:
            an.append({"code": "mbr_outside_disk", "lba": 0, "starts": [p["start_lba"]]})
    for i, a in enumerate(parts):
        for b in parts[i + 1 :]:
            if a["start_lba"] < b["start_lba"] + b["sectors"] and b["start_lba"] < a["start_lba"] + a["sectors"]:
                an.append({"code": "mbr_overlap", "lba": 0, "starts": sorted([a["start_lba"], b["start_lba"]])})
    return "mbr"


def walk_ebr(d: Disk, ext_start: int, ext_size: int, out: dict) -> None:
    """Entrada 1 relativa al EBR actual; entrada 2 (enlace) relativa al inicio de la extendida."""
    ebrs, cur, prev = [], ext_start, None
    while cur is not None and len(ebrs) < MAX_EBR:
        if cur in ebrs:
            out["anomalies"].append({"code": "ebr_loop", "lba": prev, "target_lba": cur})
            break
        if not 0 <= cur < d.n:
            out["anomalies"].append({"code": "ebr_link_outside_disk", "lba": prev, "target_lba": cur})
            break
        if not ext_start <= cur < ext_start + ext_size:
            out["anomalies"].append({"code": "ebr_link_outside_extended", "lba": prev, "target_lba": cur})
            break
        ebrs.append(cur)
        sec = d.sector(cur)
        out["checks"].append((has_sig(sec), f"EBR en {cur} sin firma"))
        t = mbr_entries(sec)
        if t[0]["type"]:
            out["partitions"].append({"start_lba": cur + t[0]["start"], "sectors": t[0]["count"],
                                      "mbr_type": t[0]["type"], "active": t[0]["boot"] == 0x80,
                                      "logical": True})
        prev, cur = cur, (ext_start + t[1]["start"] if t[1]["type"] in EXTENDED else None)
    out["ebr_lbas"] = ebrs


# --------------------------------------------------------------------------------------------- GPT


def guid(b: bytes) -> str:
    return str(uuid.UUID(bytes_le=b))


def gpt_header_at(d: Disk, lba: int) -> tuple[str, dict | None]:
    """Estado de una cabecera: valid, crc (firma presente pero CRC32 o tamaño incorrectos),
    missing (sin firma) u outside_image (la posición no existe en la imagen)."""
    sec = d.sector(lba)
    if sec is None:
        return "outside_image", None
    if sec[:8] != b"EFI PART":
        return "missing", None
    f = struct.unpack_from("<8sIIIIQQQQ16sQIII", sec, 0)
    h = dict(zip(["sig", "rev", "hsize", "crc", "res", "my", "alt", "first", "last",
                  "guid", "entries_lba", "n", "esize", "ecrc"], f))
    h["guid"] = guid(h["guid"])
    if not 92 <= h["hsize"] <= d.ss:
        return "crc", h
    raw = bytearray(sec[: h["hsize"]])
    struct.pack_into("<I", raw, 16, 0)
    return ("valid" if (zlib.crc32(bytes(raw)) & 0xFFFFFFFF) == h["crc"] else "crc"), h


def analyze_gpt(d: Disk) -> dict:
    """Estado de cada copia, desplazamiento (LBA disco - LBA imagen) y copia elegida (UEFI 5.3.2).

    La primaria se busca en LBA 1; si no es válida, la de respaldo en el último sector y, si su MyLBA
    no coincide con su posición, de ahí se deduce el desplazamiento y se vuelve a buscar la primaria.
    """
    an = []
    pst, prim = gpt_header_at(d, 1)
    shift, plba = 0, 1
    if pst == "valid":
        blba = prim["alt"]
        bst, back = gpt_header_at(d, blba)
    else:
        blba = d.n - 1
        bst, back = gpt_header_at(d, blba)
        if bst == "valid" and back["my"] != blba:
            shift = back["my"] - blba
            plba = 1 - shift
            pst, prim = gpt_header_at(d, plba)
    g = {"shift": shift, "prim": prim, "back": back, "anomalies": an}
    for tag, st, h, lba in (("primary", pst, prim, plba), ("backup", bst, back, blba)):
        g[f"gpt_{tag}_header"] = st
        if st in ("valid", "crc"):
            g[f"gpt_{tag}_header_image_lba"] = lba
        else:
            an.append({"code": f"gpt_{tag}_header_{st}", "lba": lba})
        if st == "crc":
            an.append({"code": f"gpt_{tag}_header_crc", "lba": lba})
        est = "unchecked"
        if st == "valid":
            raw = d.bytes_at(h["entries_lba"] - shift, h["n"] * h["esize"])
            if raw is None:
                est = "outside_image"
            else:
                est = "valid" if (zlib.crc32(raw) & 0xFFFFFFFF) == h["ecrc"] else "crc"
                g[f"{tag}_table"] = raw
            if est != "valid":
                an.append({"code": f"gpt_{tag}_entries_{est}", "lba": h["entries_lba"] - shift})
        g[f"gpt_{tag}_entries"] = est
    if pst == "valid" and g["gpt_primary_entries"] == "valid":
        g["gpt_selected"], sel = "primary", prim
    elif bst == "valid" and g["gpt_backup_entries"] == "valid":
        g["gpt_selected"], sel = "backup", back
    else:
        g["gpt_selected"], sel = "none", None
    g["present"] = "gpt_primary_header_image_lba" in g or "gpt_backup_header_image_lba" in g
    if shift:
        an.append({"code": "image_shifted", "lba": -shift, "value": shift})
    g["trailing"], g["entries"] = 0, []
    if sel is not None:
        sel_lba = g[f"gpt_{g['gpt_selected']}_header_image_lba"]
        if sel["first"] - shift < 0 or sel["last"] - shift > d.n - 1:
            an.append({"code": "gpt_usable_range_outside_image", "lba": sel_lba})
        disk_last = prim["alt"] if g["gpt_selected"] == "primary" else back["my"]
        g["trailing"] = max(0, disk_last - shift - (d.n - 1))
        g["entries"] = parse_gpt_entries(g[f"{g['gpt_selected']}_table"], sel)
    return g


def parse_gpt_entries(raw: bytes, h: dict) -> list[dict]:
    found = []
    for i in range(h["n"]):
        e = raw[i * h["esize"] : (i + 1) * h["esize"]]
        if e[:16] == b"\x00" * 16:
            continue
        start, end, _attrs = struct.unpack_from("<QQQ", e, 32)
        found.append({"start_lba": start, "sectors": end - start + 1, "end_lba": end,
                      "gpt_type": guid(e[0:16]), "unique_guid": guid(e[16:32]),
                      "gpt_name": e[56:128].decode("utf-16-le").rstrip("\x00"),
                      "in_range": h["first"] <= start <= end <= h["last"]})
    return found


# ------------------------------------------------------------------------ sectores de arranque FS


def exfat_sum(region: bytes) -> int:
    c = 0
    for i, b in enumerate(region):
        if i not in (106, 107, 112):
            c = ((0x80000000 if c & 1 else 0) + (c >> 1) + b) & 0xFFFFFFFF
    return c


def boot_kind(sec: bytes | None, ss: int) -> dict | None:
    """Reconoce un arranque NTFS, FAT32 o exFAT coherente; devuelve sus campos o None."""
    if sec is None or sec[0x1FE:0x200] != b"\x55\xaa":
        return None
    if sec[3:11] == b"NTFS    ":
        bps, spc, rsvd = struct.unpack_from("<HBH", sec, 0x0B)
        hidden = struct.unpack_from("<I", sec, 0x1C)[0]
        total, mft, mirr = struct.unpack_from("<QQQ", sec, 0x28)
        if bps != ss or spc == 0 or spc & (spc - 1) or rsvd != 0 or total == 0:
            return None
        if not (0 < mft * spc < total and 0 < mirr * spc < total):
            return None
        return {"fs": "ntfs", "hidden": hidden, "sectors": total + 1, "backup_off": total,
                "serial": f"{struct.unpack_from('<Q', sec, 0x48)[0]:016x}", "mbr_type": 0x07}
    if sec[0x52:0x5A] == b"FAT32   ":
        bps, spc, rsvd, nfats, root_ent, tot16, _media, fatsz16 = struct.unpack_from("<HBHBHHBH", sec, 0x0B)
        hidden, tot32, fatsz32 = struct.unpack_from("<III", sec, 0x1C)
        bk = struct.unpack_from("<H", sec, 0x32)[0]
        if bps != ss or spc == 0 or spc & (spc - 1) or rsvd == 0 or nfats == 0:
            return None
        if root_ent or tot16 or fatsz16 or not tot32 or not fatsz32:
            return None
        clusters = (tot32 - rsvd - nfats * fatsz32) // spc
        if clusters < 65525 or (clusters + 2) * 4 > fatsz32 * bps:
            return None
        return {"fs": "fat32", "hidden": hidden, "sectors": tot32, "backup_off": bk,
                "fsinfo": struct.unpack_from("<H", sec, 0x30)[0],
                "serial": f"{struct.unpack_from('<I', sec, 0x43)[0]:08x}", "mbr_type": 0x0C}
    if sec[3:11] == b"EXFAT   ":
        if any(sec[11:64]):
            return None
        off, length, fo, fl, ho, cc, root, serial, _rev, _flags, bshift, cshift, nf = struct.unpack_from(
            "<QQIIIIIIHHBBB", sec, 64)
        if 1 << bshift != ss or nf not in (1, 2) or length < (1 << 20) >> bshift:
            return None
        if fo < 24 or ho < fo + fl * nf or cc > (length - ho) >> cshift or (cc + 2) * 4 > fl * ss or root < 2:
            return None
        return {"fs": "exfat", "hidden": off, "sectors": length, "backup_off": 12,
                "serial": f"{serial:08x}", "mbr_type": 0x07}
    return None


def exfat_region_ok(d: Disk, lba: int) -> bool:
    """Sectores 1-8 con firma 0xAA550000 y sector 11 lleno con la suma de los sectores 0-10."""
    region = d.bytes_at(lba, 12 * d.ss)
    if region is None:
        return False
    ss = d.ss
    for i in range(1, 9):
        if region[(i + 1) * ss - 4 : (i + 1) * ss] != b"\x00\x00\x55\xaa":
            return False
    return region[11 * ss :] == struct.pack("<I", exfat_sum(region[: 11 * ss])) * (ss // 4)


def boot_at(d: Disk, lba: int) -> dict | None:
    b = boot_kind(d.sector(lba), d.ss)
    if b and b["fs"] == "exfat" and not exfat_region_ok(d, lba):
        return None
    return b


def scan_volumes(d: Disk, checks: list) -> tuple[list[dict], int | None]:
    """Recorre todos los sectores y agrupa las copias de arranque por (FS, número de serie).

    Con las dos copias, el inicio es la primera y el desplazamiento de la imagen es
    sectores_ocultos - inicio; con una sola, el inicio se deduce de sus sectores ocultos.
    """
    groups: dict[tuple, list[tuple[int, dict]]] = {}
    for lba in range(d.n):
        b = boot_at(d, lba)
        if b:
            groups.setdefault((b["fs"], b["serial"]), []).append((lba, b))
    deltas = set()
    for (fs, _), copies in groups.items():
        if len(copies) == 2:
            (l0, b0), (l1, _) = copies
            checks.append((l1 - l0 == b0["backup_off"], f"{fs}: copias a distancia {l1 - l0}"))
            deltas.add(b0["hidden"] - l0)
        elif len(copies) > 2:
            checks.append((False, f"{fs}: {len(copies)} copias del mismo volumen"))
    checks.append((len(deltas) <= 1, f"desplazamientos de volumen incoherentes: {deltas}"))
    delta = next(iter(deltas)) if len(deltas) == 1 else None
    vols = []
    for (fs, serial), copies in sorted(groups.items(), key=lambda g: g[1][0][0]):
        lba, b = copies[0]
        if len(copies) == 2:
            start = lba
        else:
            start = b["hidden"] - (delta or 0)
            if lba != start:
                checks.append((lba == start + b["backup_off"], f"{fs} en {lba}: posición sin explicar"))
        found = {c[0] for c in copies}
        prim = "valid" if start in found else ("outside_image" if start < 0 else "invalid")
        back = "valid" if start + b["backup_off"] in found else "invalid"
        vols.append({"fs": fs, "start_lba": start, "sectors": b["sectors"], "hidden_sectors": b["hidden"],
                     "backup_boot_lba": start + b["backup_off"], "primary_boot": prim, "backup_boot": back,
                     "serial": serial, "suggested_mbr_type": b["mbr_type"]})
        if fs == "fat32":  # FSInfo y su copia
            for off in (b["fsinfo"], b["backup_off"] + b["fsinfo"]):
                s = d.sector(start + off)
                checks.append((s is not None and s[:4] == b"RRaA" and s[484:488] == b"rrAa"
                               and s[508:512] == b"\x00\x00\x55\xaa", f"FSInfo de FAT32 en {start + off}"))
        if prim == back == "valid":
            n = 12 if fs == "exfat" else 1
            checks.append((d.bytes_at(start, n * d.ss) == d.bytes_at(start + b["backup_off"], n * d.ss),
                           f"{fs}: la copia difiere del arranque principal"))
    return vols, delta


# -------------------------------------------------------------------------------------- comparación


def anomaly_key(a: dict) -> tuple:
    return (a["code"], a["lba"], a.get("target_lba"), a.get("value"), tuple(a.get("starts", ())))


GPT_KEYS = ("gpt_primary_header", "gpt_primary_header_image_lba", "gpt_primary_entries", "gpt_backup_header",
            "gpt_backup_header_image_lba", "gpt_backup_entries", "gpt_selected")
LABELLED = ("mbr_multiple_active", "mbr_unknown_type", "mbr_outside_disk", "mbr_overlap")


def validate(data: bytes, m: dict, r: Report) -> None:
    ss = m["sector_size"]
    d = Disk(data, ss)
    out = {"anomalies": [], "partitions": [], "checks": []}
    kind = analyze_mbr(d, out)

    g = analyze_gpt(d) if kind != "mbr" else None
    gpt = g is not None and (kind in ("protective", "hybrid") or g["present"])
    scheme = "hybrid" if kind == "hybrid" else "gpt" if gpt else "mbr" if kind == "mbr" else "none"
    r.check(scheme == m["scheme"], f"esquema deducido {scheme} != {m['scheme']}")

    vols, delta = scan_volumes(d, out["checks"])
    shift = 0
    if gpt:
        shift = g["shift"]
        out["anomalies"] += g["anomalies"]
        validate_gpt(m, g, out, r)
    elif delta:
        shift = delta
        out["anomalies"].append({"code": "image_shifted", "lba": -delta, "value": delta})
        tbl = d.sector(-delta)
        if tbl is not None and has_sig(tbl) and looks_like_table(tbl):
            starts = {e["start"] for e in mbr_entries(tbl) if e["type"]}
            if starts == {v["hidden_sectors"] for v in vols}:
                out["anomalies"].append({"code": "mbr_displaced", "lba": -delta})
    r.check(shift == m["leading_sectors_missing"], f"desplazamiento deducido {shift} != {m['leading_sectors_missing']}")
    trailing = g["trailing"] if gpt else 0
    r.check(trailing == m["trailing_sectors_missing"],
            f"sectores finales ausentes {trailing} != {m['trailing_sectors_missing']}")

    if kind in ("mbr", "hybrid"):
        r.check(out.get("disk_signature") == m.get("disk_signature"), "firma de disco distinta")
    if kind == "mbr":
        ext = out.get("extended")
        exp_ext = ((m["extended_start_lba"], m["extended_sectors"], m.get("extended_type", 0x0F))
                   if "extended_start_lba" in m else None)
        r.check(ext == exp_ext, f"partición extendida {ext} != {exp_ext}")
        r.check(out.get("ebr_lbas", []) == m.get("ebr_lbas", []),
                f"cadena de EBR {out.get('ebr_lbas')} != {m.get('ebr_lbas', [])}")
        compare_partitions(d, m, out["partitions"], r, ["start_lba", "sectors", "mbr_type", "active", "logical"])
    elif gpt:
        compare_partitions(d, m, g["entries"], r,
                           ["start_lba", "sectors", "end_lba", "gpt_type", "unique_guid", "gpt_name"])
    else:
        r.check(not m.get("partitions"), "el manifiesto declara particiones en un disco sin tabla")
    if kind == "hybrid":
        labels = {p["start_lba"]: p["label"] for p in m.get("partitions", [])}
        by_range = {(p["start_lba"], p["sectors"]): labels.get(p["start_lba"], "?") for p in g["entries"]}
        got = [(e["index"], e["type"], e["boot"] == 0x80, e["start"], e["count"],
                by_range.get((e["start"], e["count"]), "")) for e in out["hybrid"]]
        exp = [(h["index"], h["mbr_type"], h["active"], h["start_lba"], h["sectors"], h["gpt_label"])
               for h in m.get("hybrid_mbr_entries", [])]
        r.check(got == exp, f"entradas del MBR híbrido {got} != {exp}")

    for decoy in m.get("decoy_lbas", []):
        s = d.sector(decoy)
        r.check(s is not None and has_sig(s) and looks_like_table(s),
                f"el señuelo de LBA {decoy} no tiene aspecto de tabla")
    for cond, msg in out["checks"]:
        r.check(cond, msg)

    # Anomalías: conjunto deducido == conjunto del manifiesto (las particiones, por su LBA de inicio).
    got = sorted(anomaly_key(a) for a in out["anomalies"])
    exp = []
    for a in m.get("anomalies", []):
        if a["code"] in LABELLED:
            a = dict(a, starts=sorted(p["start_lba"] for p in m.get("partitions", [])
                                      if p["label"] in a.get("partitions", [])))
        exp.append(anomaly_key(a))
    r.check(got == sorted(exp), f"anomalías deducidas {got} != {sorted(exp)}")

    compare_volumes(d, m, vols, r)


def compare_volumes(d: Disk, m: dict, vols: list[dict], r: Report) -> None:
    unread = set(m.get("unreadable_lbas", []))
    expv = m.get("found_volumes", [])
    r.check(len(vols) == len(expv), f"nº de volúmenes {len(vols)} != {len(expv)}")
    for v, e in zip(vols, expv):
        tag = f"volumen {e['fs']}@{e['start_lba']}"
        for k in ("fs", "start_lba", "sectors", "hidden_sectors", "backup_boot_lba", "serial", "suggested_mbr_type"):
            r.check(v[k] == e[k], f"{tag}.{k}: {v[k]!r} != {e[k]!r}")
        for k, lba in (("primary_boot", v["start_lba"]), ("backup_boot", v["backup_boot_lba"])):
            want = e[k]
            if want in ("damaged", "unreadable"):
                # Dañada: contenido no reconocible. Ilegible: además figura en unreadable_lbas.
                ok = v[k] == "invalid" and (want == "unreadable") == (lba in unread)
            else:
                ok = v[k] == want
            r.check(ok, f"{tag}.{k}: {v[k]} != {want}")
        evidence = [x for x, st in (("boot", v["primary_boot"]), ("backup_boot", v["backup_boot"])) if st == "valid"]
        r.check(evidence == e["evidence"], f"{tag}: evidencias {evidence} != {e['evidence']}")
        r.check(e["end_lba"] == e["start_lba"] + e["sectors"] - 1 and e["boot_lba"] == e["start_lba"]
                and e["partial"] == (e["start_lba"] < 0), f"{tag}: campos derivados incoherentes")
    for lba in sorted(unread):  # relleno que imita la firma del FS pero no es un arranque válido
        s = d.sector(lba)
        r.check(s is not None and has_sig(s) and s[3:11] == b"NTFS    " and boot_kind(s, d.ss) is None,
                f"LBA ilegible {lba} sin el relleno de prueba")


def validate_gpt(m: dict, g: dict, out: dict, r: Report) -> None:
    for k in GPT_KEYS:
        r.check(g.get(k) == m.get(k), f"{k}: deducido {g.get(k)!r} != {m.get(k)!r}")
    prot = out.get("protective")
    if prot:
        r.check(prot["index"] == 0 and prot["start"] == 1, f"MBR protector incorrecto: {prot}")
    prim, back = g["prim"], g["back"]
    for tag, h, valid in (("primaria", prim, g["gpt_primary_header"] == "valid"),
                          ("copia", back, g["gpt_backup_header"] == "valid")):
        if not valid:
            continue
        r.check(h["rev"] == 0x00010000, f"cabecera {tag}: revisión")
        r.check(h["first"] == m["first_usable_lba"] and h["last"] == m["last_usable_lba"],
                f"cabecera {tag}: rango utilizable")
        r.check(h["guid"] == m["disk_guid"], f"cabecera {tag}: GUID de disco")
        r.check(h["n"] == m["entry_count"] and h["esize"] == m["entry_size"] and h["esize"] % 8 == 0,
                f"cabecera {tag}: número o tamaño de entradas")
    if g["gpt_primary_header"] == "valid":
        r.check(prim["my"] == 1 and prim["entries_lba"] == m["entries_lba"], "cabecera primaria: MyLBA/entradas")
        r.check(prim["alt"] == m["backup_header_lba"], "cabecera primaria: AlternateLBA")
    if g["gpt_backup_header"] == "valid":
        r.check((back["my"], back["alt"]) == (m["backup_header_lba"], 1), "cabecera de respaldo: MyLBA/AlternateLBA")
        r.check(back["entries_lba"] == m["backup_entries_lba"], "LBA de entradas de la copia")
    for tag, h in (("primary", prim), ("backup", back)):
        if g[f"gpt_{tag}_entries"] == "valid":
            r.check(f"{h['ecrc']:08x}" == m["entries_crc32"], f"CRC32 de entradas ({tag}) distinto del manifiesto")
    if g["gpt_primary_entries"] == g["gpt_backup_entries"] == "valid":
        r.check(g["primary_table"] == g["backup_table"], "la tabla de entradas de la copia difiere de la primaria")
    for e in g["entries"]:
        r.check(e["in_range"], f"entrada en {e['start_lba']} fuera del rango utilizable")


def compare_partitions(d: Disk, m: dict, found: list[dict], r: Report, keys: list[str]) -> None:
    exp = m.get("partitions", [])
    r.check(len(found) == len(exp), f"nº de particiones {len(found)} != {len(exp)}")
    shift = m["leading_sectors_missing"]
    for f, e in zip(found, exp):
        for k in keys:
            r.check(f[k] == e[k], f"{e['label']}.{k}: {f[k]!r} != {e[k]!r}")
        lba = e.get("marker_lba", e["start_lba"])
        r.check(lba == e["start_lba"] - shift, f"{e['label']}: marker_lba incoherente con el desplazamiento")
        mk = e["marker"].encode()
        sec = d.sector(lba)
        r.check(sec is not None and sec[: len(mk)] == mk, f"{e['label']}: marcador ausente en LBA {lba}")


def main(argv: list[str]) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    if len(argv) != 2:
        print(__doc__)
        return 2
    img_dir, ref_dir = Path(argv[0]), Path(argv[1])
    refs = sorted(ref_dir.glob("*.manifest.toml"))
    if not refs:
        print(f"ERROR: no hay manifiestos de referencia en {ref_dir}")
        return 1
    failed = 0
    for ref in refs:
        m = tomllib.loads(ref.read_text(encoding="utf-8"))
        r = Report(m["name"])
        path = img_dir / m["file"]
        if not path.exists():
            r.errors.append(f"falta la imagen {path}")
        elif r.check(m.get("schema_version") == SCHEMA_VERSION, "versión de esquema del manifiesto no admitida"):
            data = path.read_bytes()
            r.check(len(data) == m["size_bytes"], "tamaño distinto")
            r.check(len(data) == m["sector_size"] * m["total_sectors"], "tamaño no coincide con sectores")
            r.check(hashlib.sha256(data).hexdigest() == m["sha256"], "SHA-256 distinto (¿generación no determinista?)")
            try:
                validate(data, m, r)
            except (KeyError, TypeError, ValueError, IndexError, struct.error) as ex:
                r.errors.append(f"excepción al validar: {type(ex).__name__}: {ex}")
        status = "OK " if not r.errors else "FALLO"
        print(f"[{status}] {r.name}: {r.checks} comprobaciones, {len(r.errors)} errores")
        for e in r.errors:
            print(f"        - {e}")
        failed += bool(r.errors)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
