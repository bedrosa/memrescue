#!/usr/bin/env python3
"""Construye los manifiestos semánticos del corpus NTFS con The Sleuth Kit como oráculo.

Uso (en el ejecutor de GitHub Actions, tras generate.sh):
    python3 -I build_manifests.py <dir_generado> <dir_manifiestos_salida> [--compare-with <otro_dir_generado>]
                                  [--tsk-dump <dir>]

La verdad de cada manifiesto sale de la salida de TSK (`mmls`, `fsstat`, `istat -r`, `icat`, `ifind`):
registros MFT, estado de asignación, nombres y rutas (cadena de padres de `$FILE_NAME`), tamaños,
residencia, runs, rangos dispersos, flujos alternativos y SHA-256 del contenido que devuelve `icat`.
MemRescue no interviene. La intención que escribió el generador (`intent/*.intent.json`) solo se usa
para comprobar que TSK ve exactamente lo que se hizo (mismas rutas, inodos de ntfs-3g, tamaños y
hashes) y, en los ficheros sobrescritos, para comparar cluster a cluster con el contenido original.
Cualquier discrepancia detiene la generación.

Con `--compare-with`, `byte_reproducible` indica si cada imagen tiene el mismo SHA-256 en las dos
generaciones independientes del mismo workflow.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ntfs_tree import content  # noqa: E402  (mismo generador: reconstruye el contenido original)

SCHEMA_VERSION = 1
FAKE_TIME = "2026-01-01 00:00:00"
GENERATOR = "tests/generators/ntfs/generate.sh"
PACKAGES = ["ntfs-3g", "libfaketime", "sleuthkit", "util-linux"]
MBR_SIGNATURE = "4d524e31"
GPT_DISK_GUID = "4d52a001-0000-4000-8000-000000000001"
GPT_PART_GUID = "4d52a001-0000-4000-8000-0000000000a1"
GPT_BASIC_DATA = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"

SCENARIOS = {
    "ntfs_basic": {"desc": "Volumen NTFS de 32 MiB sin tabla de particiones: árbol de 3 niveles con 52 ficheros residentes y no residentes"},
    "ntfs_deleted": {"desc": "ntfs_basic con 5 ficheros y 1 directorio (con 5 ficheros) borrados sin sobrescribir"},
    "ntfs_overwritten": {"desc": "ntfs_basic, volumen lleno con /relleno.bin, 3 ficheros borrados cuyos clusters reutilizan dos ficheros nuevos y 2 borrados intactos"},
    "ntfs_fragmented": {"desc": "4 ficheros escritos a trozos de 8 KiB intercalados (fragmentados) y 2 contiguos de control"},
    "ntfs_sparse_ads": {"desc": "Ficheros dispersos (hueco inicial, intermedio, final, perforado y solo hueco) y flujos alternativos en ficheros y en un directorio"},
    "ntfs_damaged_boot": {"desc": "ntfs_basic con el sector de arranque principal sobrescrito; la copia del final está intacta", "base": "ntfs_basic"},
    "ntfs_damaged_mft": {"desc": "ntfs_basic con 4 registros MFT de ficheros sobrescritos", "base": "ntfs_basic"},
    "ntfs_damaged_mftmirr": {"desc": "ntfs_basic con los 4 registros de $MFTMirr sobrescritos", "base": "ntfs_basic"},
    "ntfs_mbr": {"desc": "Disco MBR con una partición NTFS (0x07, activa) en la LBA 2048 con el árbol de ntfs_basic", "scheme": "mbr", "base": "ntfs_basic"},
    "ntfs_gpt": {"desc": "Disco GPT con una partición de datos básicos NTFS en la LBA 2048 con el árbol de ntfs_basic", "scheme": "gpt", "base": "ntfs_basic"},
}
DAMAGE_EXPECTED = {
    "boot": ("El sector de arranque principal no es válido y la copia de la última LBA del volumen sí. "
             "Un lector tolerante debe localizar la copia, usarla e informar del daño. TSK no abre la imagen "
             "tal cual; la verdad de [[files]] es la que da TSK con la copia restaurada en una copia temporal."),
    "mft": ("4 registros MFT de ficheros sin firma FILE. El resto del árbol debe leerse igual que en ntfs_basic "
            "y esos ficheros deben informarse como dañados (mft_damaged), sin inventar contenido."),
    "mftmirr": ("$MFTMirr no coincide con $MFT. La lectura del árbol no cambia (se usa $MFT) y la discrepancia "
                "debe informarse."),
}


class Fail(Exception):
    pass


# ------------------------------------------------------------------------------------- TSK


class Tsk:
    def __init__(self, image: Path, offset: int, dump: Path | None) -> None:
        self.image, self.offset, self.dump = image, offset, dump
        self.cache: dict[int, dict | None] = {}
        self.use_runs = True

    def run(self, args: list[str], binary: bool = False, check: bool = True):
        r = subprocess.run(args, capture_output=True)
        if check and r.returncode != 0:
            raise Fail(f"{' '.join(args)}: {r.stderr.decode('utf-8', 'replace').strip()[:400]}")
        return r if binary else r.stdout.decode("utf-8", "replace")

    def save(self, name: str, text: str) -> None:
        if self.dump:
            self.dump.mkdir(parents=True, exist_ok=True)
            (self.dump / name).write_text(text, encoding="utf-8")

    def base(self, tool: str) -> list[str]:
        return [tool, "-f", "ntfs", "-o", str(self.offset)]

    def fsstat(self) -> str:
        out = self.run(self.base("fsstat") + [str(self.image)])
        self.save("fsstat.txt", out)
        return out

    def opens(self) -> bool:
        return self.run(self.base("fsstat") + [str(self.image)], binary=True, check=False).returncode == 0

    def istat(self, n: int) -> dict | None:
        if n in self.cache:
            return self.cache[n]
        args = self.base("istat") + ["-z", "UTC"] + (["-r"] if self.use_runs else []) + [str(self.image), str(n)]
        r = self.run(args, binary=True, check=False)
        err = r.stderr.decode("utf-8", "replace")
        if r.returncode != 0 and self.use_runs and ("invalid option" in err or "illegal option" in err):
            self.use_runs = False
            return self.istat(n)
        out = r.stdout.decode("utf-8", "replace")
        self.save(f"istat_{n:05d}.txt", out + ("\n# stderr\n" + err if err else ""))
        info = parse_istat(out, self.use_runs) if r.returncode == 0 else None
        self.cache[n] = info
        return info

    def icat(self, n: int, type_id: int, attr_id: int) -> bytes:
        r = self.run(self.base("icat") + [str(self.image), f"{n}-{type_id}-{attr_id}"], binary=True)
        return r.stdout

    def ifind_block(self, lcn: int) -> int | None:
        out = self.run(self.base("ifind") + ["-d", str(lcn), str(self.image)], check=False)
        m = re.match(r"\s*(\d+)", out)
        return int(m.group(1)) if m else None


def mmls(image: Path, dump: Path | None) -> dict:
    out = subprocess.run(["mmls", str(image)], capture_output=True, check=True).stdout.decode("utf-8", "replace")
    if dump:
        (dump / "mmls.txt").write_text(out, encoding="utf-8")
    rows = []
    for line in out.splitlines():
        m = re.match(r"^\s*\d+:\s+(\d+(?::\d+)?)\s+(\d+)\s+(\d+)\s+(\d+)\s+(.*?)\s*$", line)
        if m:
            rows.append({"start": int(m.group(2)), "end": int(m.group(3)), "length": int(m.group(4)),
                         "description": m.group(5)})
    if len(rows) != 1:
        raise Fail(f"mmls debe ver exactamente 1 partición y ve {len(rows)}:\n{out}")
    first = out.splitlines()[0].strip() if out else ""
    return {**rows[0], "table": first}


def parse_istat(text: str, runs_mode: bool) -> dict:
    info = {"entry": None, "sequence": None, "allocated": None, "fns": [], "attrs": [], "times": []}
    section = None
    attr = None
    fn = None
    for raw in text.splitlines():
        line = raw.rstrip()
        m = re.match(r"^Entry:\s*(\d+)\s+Sequence:\s*(\d+)", line)
        if m:
            info["entry"], info["sequence"] = int(m.group(1)), int(m.group(2))
            continue
        if re.match(r"^Not Allocated", line):
            info["allocated"] = False
            continue
        if re.match(r"^Allocated", line):
            info["allocated"] = True
            continue
        if line.startswith("$STANDARD_INFORMATION Attribute Values"):
            section = "si"
            continue
        if line.startswith("$FILE_NAME Attribute Values"):
            section = "fn"
            fn = {"name": None, "parent": None, "parent_seq": None}
            info["fns"].append(fn)
            continue
        if line.startswith("$OBJECT_ID") or line.startswith("$ATTRIBUTE_LIST"):
            section = "other"
            continue
        if line.startswith("Attributes:"):
            section = "attrs"
            continue
        if section == "si":
            m = re.match(r"^(Created|File Modified|MFT Modified|Accessed):\s+(.*)$", line)
            if m:
                info["times"].append(m.group(2).strip())
        elif section == "fn" and fn is not None:
            m = re.match(r"^Name:\s(.*)$", line)
            if m and fn["name"] is None:
                fn["name"] = m.group(1)
            m = re.match(r"^Parent MFT Entry:\s*(\d+)\s+Sequence:\s*(\d+)", line)
            if m:
                fn["parent"], fn["parent_seq"] = int(m.group(1)), int(m.group(2))
        elif section == "attrs":
            m = re.match(r"^Type:\s+(\$\S+)\s+\((\d+)-(\d+)\)\s+Name:\s+(.*?)\s+(Non-Resident|Resident)(.*?)\s+size:\s+(\d+)(?:\s+init_size:\s+(\d+))?", line)
            if m:
                attr = {"type": m.group(1), "type_id": int(m.group(2)), "id": int(m.group(3)),
                        "name": "" if m.group(4) == "N/A" else m.group(4),
                        "resident": m.group(5) == "Resident", "flags": m.group(6).strip(" ,"),
                        "size": int(m.group(7)),
                        "init_size": int(m.group(8)) if m.group(8) is not None else None,
                        "raw_runs": [], "raw": []}
                info["attrs"].append(attr)
                continue
            if attr is not None and line.strip():
                attr["raw"].append(line.strip())
                if runs_mode:
                    m = re.search(r"Starting address:\s*(\S+?),\s*length:\s*(\d+)(.*)$", line)
                    if m:
                        addr = m.group(1)
                        sparse = "sparse" in m.group(3).lower() or not addr.isdigit()
                        attr["raw_runs"].append((None if sparse else int(addr), int(m.group(2))))
                else:
                    for tok in line.split():
                        if tok.isdigit():
                            lcn = int(tok)
                            attr["raw_runs"].append((None if lcn == 0 else lcn, 1))
    return info


def normalize_runs(raw: list[tuple[int | None, int]]) -> tuple[list[list[int]], list[list[int]]]:
    """Runs contiguos fusionados: ([vcn, lcn, clusters] asignados, [vcn, clusters] dispersos)."""
    merged: list[list] = []
    vcn = 0
    for lcn, n in raw:
        if n <= 0:
            continue
        if merged:
            pv, pl, pn = merged[-1]
            if (pl is None and lcn is None) or (pl is not None and lcn is not None and pl + pn == lcn):
                merged[-1][2] += n
                vcn += n
                continue
        merged.append([vcn, lcn, n])
        vcn += n
    runs = [[v, l, n] for v, l, n in merged if l is not None]
    sparse = [[v, n] for v, l, n in merged if l is None]
    return runs, sparse


def choose_fn(fns: list[dict]) -> dict | None:
    good = [f for f in fns if f["name"] is not None and f["parent"] is not None]
    if not good:
        return None
    long_names = [f for f in good if "~" not in f["name"]] or good
    return max(long_names, key=lambda f: len(f["name"]))


# ----------------------------------------------------------------------------- análisis


def fsstat_fields(text: str) -> dict:
    def grab(rx: str, cast=str):
        m = re.search(rx, text, re.M)
        if not m:
            raise Fail(f"fsstat sin «{rx}»:\n{text}")
        return cast(m.group(1).strip())
    first, last = grab(r"^Range:\s*(\d+\s*-\s*\d+)").split("-")
    s_first, s_last = grab(r"^Total Sector Range:\s*(\d+\s*-\s*\d+)").split("-")
    return {
        "serial": grab(r"^Volume Serial Number:\s*([0-9A-Fa-f]+)").lower(),
        "label": grab(r"^Volume Name:\s*(.*)$"),
        "version": grab(r"^Version:\s*(.*)$"),
        "mft_lcn": grab(r"^First Cluster of MFT:\s*(\d+)", int),
        "mftmirr_lcn": grab(r"^First Cluster of MFT Mirror:\s*(\d+)", int),
        "mft_record_size": grab(r"^Size of MFT Entries:\s*(\d+)", int),
        "index_record_size": grab(r"^Size of Index Records:\s*(\d+)", int),
        "mft_entries": int(last) + 1,
        "sector_size": grab(r"^Sector Size:\s*(\d+)", int),
        "cluster_size": grab(r"^Cluster Size:\s*(\d+)", int),
        "total_sectors": int(s_last) - int(s_first) + 1,
    }


def analyze(tsk: Tsk) -> tuple[dict, dict[int, dict]]:
    vol = fsstat_fields(tsk.fsstat())
    records: dict[int, dict] = {}
    for n in range(vol["mft_entries"]):
        info = tsk.istat(n)
        if info is not None and info["entry"] == n:
            records[n] = info
    mft = records.get(0)
    if not mft:
        raise Fail("istat no lee el registro 0 ($MFT)")
    data = [a for a in mft["attrs"] if a["type"] == "$DATA" and not a["name"]]
    runs, sparse = normalize_runs(data[0]["raw_runs"]) if data else ([], [])
    if not runs or sparse:
        raise Fail(f"runs de $MFT no interpretables: {data[0]['raw'] if data else 'sin $DATA'}")
    vol["mft_runs"] = runs
    return vol, records


def resolve_path(n: int, records: dict[int, dict]) -> str | None:
    parts = []
    seen = set()
    while n != 5:
        if n in seen or n not in records:
            return None
        seen.add(n)
        fn = choose_fn(records[n]["fns"])
        if fn is None:
            return None
        parts.append(fn["name"])
        n = fn["parent"]
    return "/" + "/".join(reversed(parts))


def user_records(records: dict[int, dict]) -> dict[str, list[int]]:
    paths: dict[str, list[int]] = {}
    for n, info in records.items():
        if n < 16 or not info["fns"]:
            continue
        p = resolve_path(n, records)
        if p is None:
            raise Fail(f"registro {n}: ruta no resoluble con la cadena de padres de $FILE_NAME")
        if p.split("/")[1].startswith("$"):
            continue
        paths.setdefault(p, []).append(n)
    return paths


def is_dir(info: dict) -> bool:
    return any(a["type"] == "$INDEX_ROOT" for a in info["attrs"])


def stream_entry(tsk: Tsk, n: int, attr: dict, cluster: int) -> tuple[dict, bytes]:
    data = tsk.icat(n, attr["type_id"], attr["id"])
    if len(data) != attr["size"]:
        raise Fail(f"registro {n}: icat devuelve {len(data)} B y el atributo dice {attr['size']}")
    e = {"size": attr["size"], "sha256": hashlib.sha256(data).hexdigest(), "resident": attr["resident"]}
    if not attr["resident"]:
        runs, sparse = normalize_runs(attr["raw_runs"])
        needed = -(-attr["size"] // cluster)
        covered = sum(r[2] for r in runs) + sum(s[1] for s in sparse)
        if covered < needed:
            raise Fail(f"registro {n}: runs de TSK cubren {covered} clusters y hacen falta {needed}: {attr['raw']}")
        e["runs"] = runs
        if sparse:
            e["sparse"] = sparse
        if attr["init_size"] is not None and attr["init_size"] != attr["size"]:
            e["init_size"] = attr["init_size"]
    return e, data


def classify(tsk: Tsk, n: int, entry: dict, data: bytes, spec: dict, cluster: int, by_record: dict[int, str]) -> None:
    """Compara cluster a cluster lo que devuelve TSK con el contenido original del generador."""
    original = content(spec)
    total, bad, owners = 0, 0, set()
    for vcn, lcn, count in entry.get("runs", []):
        for i in range(count):
            start = (vcn + i) * cluster
            if start >= len(original):
                break
            total += 1
            if data[start:start + cluster] != original[start:start + cluster]:
                bad += 1
                owner = tsk.ifind_block(lcn + i)
                if owner is not None and owner != n:
                    owners.add(by_record.get(owner, f"registro {owner}"))
    if total == 0:
        raise Fail(f"registro {n}: contenido distinto del original sin clusters que comparar")
    entry["state"] = "overwritten"
    entry["recoverable"] = "none" if bad == total else ("partial" if bad else "full")
    entry["recovered_sha256"] = entry["sha256"]
    entry["sha256"] = hashlib.sha256(original).hexdigest()
    entry["clusters"] = total
    entry["overwritten_clusters"] = bad
    who = ", ".join(sorted(owners)) if owners else "ningún fichero asignado"
    entry["reason"] = f"{bad} de {total} clusters con otro contenido; asignados ahora a {who}"


def build_files(tsk: Tsk, vol: dict, records: dict[int, dict], intent: dict, errors: list[str]) -> list[dict]:
    cluster = vol["cluster_size"]
    paths = user_records(records)
    by_record = {n: p for p, ns in paths.items() for n in ns}
    wanted = {e["path"]: e for e in intent["entries"]}
    for p in sorted(set(paths) - set(wanted)):
        errors.append(f"TSK ve {p} (registros {paths[p]}) y el generador no lo creó")
    files = []
    for path in sorted(wanted):
        want = wanted[path]
        ns = paths.get(path, [])
        if len(ns) != 1:
            errors.append(f"{path}: TSK lo ve en {len(ns)} registros ({ns})")
            continue
        n = ns[0]
        info = records[n]
        if n != want["inode"]:
            errors.append(f"{path}: TSK lo ve en el registro {n} y ntfs-3g dio el inodo {want['inode']}")
        live = want["expect"] == "live"
        if info["allocated"] is not live:
            errors.append(f"{path}: asignado={info['allocated']} y se esperaba {want['expect']}")
        bad_times = [t for t in info["times"] if not t.startswith(FAKE_TIME)]
        if bad_times or not info["times"]:
            errors.append(f"{path}: marcas de tiempo no fijadas por libfaketime: {info['times']}")
        typ = "dir" if is_dir(info) else "file"
        if typ != want["type"]:
            errors.append(f"{path}: TSK dice {typ} y el generador creó {want['type']}")
        e = {"path": path, "type": typ, "mft_record": n, "sequence": info["sequence"],
             "state": "live" if info["allocated"] else "deleted"}
        streams = [a for a in info["attrs"] if a["type"] == "$DATA"]
        main = [a for a in streams if not a["name"]]
        if typ == "file":
            if len(main) != 1:
                errors.append(f"{path}: {len(main)} atributos $DATA sin nombre")
                continue
            s, data = stream_entry(tsk, n, main[0], cluster)
            e.update(s)
            e["recoverable"] = "full"
            if s["size"] != want["size"]:
                errors.append(f"{path}: TSK {s['size']} B, generador {want['size']} B")
            if s["sha256"] != want["sha256"]:
                if live:
                    errors.append(f"{path}: el contenido de TSK no coincide con el escrito")
                elif s["resident"]:
                    errors.append(f"{path}: borrado residente con contenido distinto")
                else:
                    classify(tsk, n, e, data, want["content"], cluster, by_record)
            if want["expect"] == "overwrite_target" and e["recoverable"] == "full":
                errors.append(f"{path}: debía quedar sobrescrito y TSK lo recupera íntegro")
            if want["expect"] == "deleted" and e["recoverable"] != "full":
                errors.append(f"{path}: borrado sin sobrescribir y TSK no lo recupera íntegro")
            if want.get("expect_fragmented") and len(e.get("runs", [])) < 2:
                errors.append(f"{path}: debía quedar fragmentado y TSK ve {e.get('runs')}")
        named = sorted((a for a in streams if a["name"]), key=lambda a: a["name"])
        want_ads = {a["name"]: a for a in want["ads"]}
        if [a["name"] for a in named] != sorted(want_ads):
            errors.append(f"{path}: flujos de TSK {[a['name'] for a in named]} y del generador {sorted(want_ads)}")
        ads = []
        for a in named:
            s, _ = stream_entry(tsk, n, a, cluster)
            w = want_ads.get(a["name"])
            if w and (w["size"], w["sha256"]) != (s["size"], s["sha256"]):
                errors.append(f"{path}:{a['name']}: tamaño o contenido distinto del escrito")
            ads.append({"name": a["name"], **s})
        if ads:
            e["ads"] = ads
        files.append(e)
    return files


def repaired_copy(image: Path, offset: int, total_bytes: int, tmp: Path) -> tuple[Path, int]:
    """Copia temporal con el arranque principal restaurado desde la copia de la última LBA."""
    data = bytearray(image.read_bytes())
    backup_at = offset + total_bytes - 512
    backup = bytes(data[backup_at:backup_at + 512])
    if backup[3:11] != b"NTFS    " or backup[510:512] != b"\x55\xaa":
        raise Fail("la copia del sector de arranque no es NTFS")
    data[offset:offset + 512] = backup
    out = tmp / image.name
    out.write_bytes(bytes(data))
    return out, backup_at


# --------------------------------------------------------------------------------- salida


def toml_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False)
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    raise TypeError(type(v))


def render(m: dict) -> str:
    lines = [
        "# Generado por tests/generators/ntfs/build_manifests.py con The Sleuth Kit como oráculo: no editar a mano.",
        "# Esquema en docs/PRUEBAS.md (§1.2). Los runs son [vcn, lcn, clusters] y los dispersos [vcn, clusters].",
    ]
    tables = {k: v for k, v in m.items() if isinstance(v, (dict, list)) and k in ("partition", "volume", "damage", "files")}
    for k, v in m.items():
        if k not in tables:
            lines.append(f"{k} = {toml_value(v)}")
    for key in ("partition", "volume"):
        if key in tables:
            lines += ["", f"[{key}]"] + [f"{k} = {toml_value(v)}" for k, v in tables[key].items()]
    for d in tables.get("damage", []):
        lines += ["", "[[damage]]"] + [f"{k} = {toml_value(v)}" for k, v in d.items()]
    for f in tables.get("files", []):
        lines += ["", "[[files]]"] + [f"{k} = {toml_value(v)}" for k, v in f.items() if k != "ads"]
        for a in f.get("ads", []):
            lines += ["", "[[files.ads]]"] + [f"{k} = {toml_value(v)}" for k, v in a.items()]
    return "\n".join(lines) + "\n"


def tool_versions() -> list[str]:
    out = subprocess.run(["dpkg-query", "-W", "-f", "${Package} ${Version}\n"] + PACKAGES,
                         capture_output=True, text=True, check=True).stdout
    return sorted(line.strip() for line in out.splitlines() if line.strip())


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def build_one(name: str, gen: Path, other: Path | None, dump: Path | None, done: dict, tools: list[str]) -> dict:
    sc = SCENARIOS[name]
    image = gen / "images" / f"{name}.img"
    intent = json.loads((gen / "intent" / f"{name}.intent.json").read_text(encoding="utf-8"))
    errors: list[str] = []
    sha = sha256_file(image)
    m: dict = {"schema_version": SCHEMA_VERSION, "name": name, "file": image.name, "scenario": sc["desc"],
               "generator": GENERATOR, "tools": tools, "fake_time": f"{FAKE_TIME} UTC",
               "image_size": image.stat().st_size, "image_sha256": sha}
    if other is not None:
        m["byte_reproducible"] = sha256_file(other / "images" / image.name) == sha
    if "base" in sc:
        m["base"] = sc["base"]
    offset_sectors = 0
    if "scheme" in sc:
        part = mmls(image, dump)
        offset_sectors = part["start"]
        m["partition"] = {"scheme": sc["scheme"], "index": 0, "start_lba": part["start"], "sectors": part["length"],
                          "tsk_table": part["table"], "tsk_description": part["description"]}
        if sc["scheme"] == "mbr":
            m["partition"].update({"disk_signature": MBR_SIGNATURE, "mbr_type": 7, "active": True})
        else:
            m["partition"].update({"disk_guid": GPT_DISK_GUID, "gpt_type": GPT_BASIC_DATA,
                                   "unique_guid": GPT_PART_GUID, "gpt_name": "MemRescue NTFS"})
    offset = offset_sectors * 512
    tsk = Tsk(image, offset_sectors, dump)
    damage = intent.get("damage")
    with tempfile.TemporaryDirectory() as tmp:
        if damage and damage["kind"] == "boot":
            if tsk.opens():
                errors.append("TSK abre la imagen con el arranque principal dañado")
            fixed, backup_at = repaired_copy(image, offset, done["ntfs_basic"]["image_size"], Path(tmp))
            tsk = Tsk(fixed, offset_sectors, dump)
        vol, records = analyze(tsk)
        vol = {"offset": offset, **vol}
        vol["backup_boot_offset"] = offset + vol["total_sectors"] * vol["sector_size"]
        m["volume"] = vol
        if damage:
            d = {"kind": damage["kind"], "regions": [[r["offset"], r["length"]] for r in damage["regions"]],
                 "description": "; ".join(r["what"] for r in damage["regions"]),
                 "expected": DAMAGE_EXPECTED[damage["kind"]],
                 "tsk_opens": damage["kind"] != "boot"}
            if damage["kind"] == "mft":
                d["records"] = [r["record"] for r in damage["regions"]]
            m["damage"] = [d]
        if damage and damage["kind"] == "mft":
            lost = {r["record"]: r["path"] for r in damage["regions"]}
            unreadable = sorted(n for n in lost if records.get(n) is None)
            if unreadable != sorted(lost):
                errors.append(f"TSK lee registros que debían estar dañados: {sorted(set(lost) - set(unreadable))}")
            m["damage"][0]["tsk_unreadable_records"] = unreadable
            intent = dict(intent, entries=[e for e in intent["entries"] if e["path"] not in lost.values()])
            files = build_files(tsk, vol, records, intent, errors)
            base_files = {f["path"]: f for f in done["ntfs_basic"]["files"]}
            for p in lost.values():
                files.append({**base_files[p], "mft_damaged": True})
            files.sort(key=lambda f: f["path"])
        else:
            files = build_files(tsk, vol, records, intent, errors)
    m["files"] = files
    if "base" in sc and sc["base"] in done:
        base = done[sc["base"]]
        strip = (lambda f: {k: v for k, v in f.items() if k != "mft_damaged"}) if "scheme" not in sc else \
            (lambda f: {k: f.get(k) for k in ("path", "type", "state", "size", "sha256", "ads")})
        if [strip(f) for f in files] != [strip(f) for f in base["files"]]:
            errors.append(f"[[files]] no coincide con {sc['base']} fuera de lo dañado")
    live = [f for f in files if f["state"] == "live"]
    m["entries_live"] = len(live)
    m["entries_deleted"] = len(files) - len(live)
    m["entries_overwritten"] = sum(1 for f in files if f["state"] == "overwritten")
    if errors:
        raise Fail(f"{name}:\n  " + "\n  ".join(errors))
    return m


def main() -> int:
    args = sys.argv[1:]
    other = dump = None
    if "--compare-with" in args:
        i = args.index("--compare-with")
        other = Path(args[i + 1])
        del args[i:i + 2]
    if "--tsk-dump" in args:
        i = args.index("--tsk-dump")
        dump = Path(args[i + 1])
        del args[i:i + 2]
    if len(args) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    gen, out = Path(args[0]), Path(args[1])
    out.mkdir(parents=True, exist_ok=True)
    tools = tool_versions()
    done: dict[str, dict] = {}
    failed = False
    for name in SCENARIOS:
        try:
            m = build_one(name, gen, other, dump / name if dump else None, done, tools)
        except Fail as e:
            print(f"ERROR {e}", file=sys.stderr)
            failed = True
            continue
        done[name] = m
        (out / f"{name}.manifest.toml").write_text(render(m), encoding="utf-8", newline="\n")
        live, deleted, over = m["entries_live"], m["entries_deleted"], m["entries_overwritten"]
        print(f"OK {name}: {live} vivos, {deleted} borrados ({over} sobrescritos), sha256 {m['image_sha256'][:16]}"
              + (f", byte_reproducible={m['byte_reproducible']}" if "byte_reproducible" in m else ""))
    if dump and dump.exists():
        shutil.make_archive(str(dump), "zip", dump)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
