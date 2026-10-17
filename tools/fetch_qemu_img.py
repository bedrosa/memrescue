#!/usr/bin/env python3
"""Obtiene qemu-img portable para pruebas, verificando procedencia e integridad. Solo para desarrollo.

Uso: python -I tools/fetch_qemu_img.py <directorio_destino>   (normalmente .tools/qemu)

Cadena de confianza (ver docs/HERRAMIENTAS_TERCEROS.md):
  1. Claves de firma de MSYS2 desde el repositorio oficial github.com/msys2/MSYS2-keyring (HTTPS),
     fijado a un commit concreto y con el SHA-256 de cada fichero comprobado; solo se aceptan las
     huellas de `msys2-trusted`.
  2. Base de datos del repositorio ucrt64 de MSYS2 (`ucrt64.db`) verificada con su firma GPG.
  3. Cada paquete: SHA-256 igual al de la base de datos firmada **y** firma GPG propia válida.
Solo se extraen `qemu-img.exe`, las DLL y las licencias. No se ejecuta nada descargado; no se toca el
PATH ni el registro; no requiere administrador.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import urllib.request
from compression import zstd
from pathlib import Path

ROOT_PACKAGE = "mingw-w64-ucrt-x86_64-qemu-image-util"
MIRROR = "https://repo.msys2.org/mingw/ucrt64/"
# Alternativas si el repositorio principal no responde. La integridad no depende del espejo: todo se
# verifica por firma GPG y por el SHA-256 de la base de datos firmada.
FALLBACK_MIRRORS = ["https://mirror.msys2.org/mingw/ucrt64/"]
# Commit del repositorio de claves y SHA-256 esperados de sus ficheros. Para actualizarlos, revisar el
# cambio en github.com/msys2/MSYS2-keyring y anotar el nuevo commit en docs/HERRAMIENTAS_TERCEROS.md.
KEYRING_COMMIT = "5d249077a0cfeec9dbf51515e5e01da413d28859"
KEYRING_SHA256 = {
    "msys2.gpg": "1257d4ccc536c53333445a618039af4c3ea3f56b96601de9af89151ea529c19a",
    "msys2-trusted": "a8d39040a7b6cc14bf4394b5d9c838443925c32b00a7d34e8729c972392dca04",
}
KEYRING_BASE = f"https://raw.githubusercontent.com/msys2/MSYS2-keyring/{KEYRING_COMMIT}/"
GPG = r"C:\Program Files\Git\usr\bin\gpg.exe"
UA = {"User-Agent": "MemRescue-dev-tools/1.0"}


def fetch(url: str) -> bytes:
    urls = [url]
    if url.startswith(MIRROR):
        urls += [m + url[len(MIRROR):] for m in FALLBACK_MIRRORS]
    last = None
    for u in urls:
        for attempt in range(3):
            try:
                req = urllib.request.Request(u, headers=UA)
                with urllib.request.urlopen(req, timeout=60) as r:
                    return r.read()
            except OSError as e:  # URLError y timeouts derivan de OSError
                last = e
                print(f"   reintento {attempt + 1} ({u.rsplit('/', 1)[-1]}): {e}")
    raise SystemExit(f"No se pudo descargar {url}: {last}")


def fetch_cached(path: Path, url: str, expect_sha256: str | None = None) -> bytes:
    """Reutiliza un fichero ya descargado si su SHA-256 coincide con el esperado."""
    if expect_sha256 and path.exists():
        data = path.read_bytes()
        if sha256(data) == expect_sha256:
            return data
    data = fetch(url)
    path.write_bytes(data)
    return data


def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def msys(p: Path) -> str:
    """Ruta en formato MSYS (/c/…), que es la que entiende el gpg de Git for Windows."""
    s = p.resolve().as_posix()
    if len(s) > 1 and s[1] == ":":
        s = "/" + s[0].lower() + s[2:]
    return s


def gpg(home: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([GPG, "--homedir", msys(home), "--batch", "--no-tty", *args],
                          capture_output=True, text=True, encoding="utf-8", errors="replace")


MIN_MASTER_CERTIFICATIONS = 2
_signer_cache: dict[str, str] = {}


def signer_is_trusted(home: Path, primary: str, trusted: set[str]) -> str:
    """Devuelve la justificación si `primary` es una clave maestra o está certificada por al menos
    MIN_MASTER_CERTIFICATIONS claves maestras distintas (modelo de confianza de pacman/MSYS2)."""
    if primary in trusted:
        return "clave maestra"
    if primary in _signer_cache:
        return _signer_cache[primary]
    r = gpg(home, "--with-colons", "--check-sigs", primary)
    lines = r.stdout.splitlines()
    pub = next((l.split(":") for l in lines if l.startswith("pub:")), None)
    if pub is None or pub[1] in ("r", "e", "i", "d"):
        raise SystemExit(f"La clave firmante {primary} no existe, está revocada, caducada o no es válida")
    by_keyid = {f[-16:]: f for f in trusted}
    certifiers = set()
    in_uid = False
    for l in lines:
        f = l.split(":")
        if f[0] == "uid":
            in_uid = True
        elif f[0] in ("sub", "pub"):
            in_uid = False
        elif f[0] == "sig" and in_uid and f[1] == "!" and f[4] in by_keyid:
            certifiers.add(by_keyid[f[4]])
    if len(certifiers) < MIN_MASTER_CERTIFICATIONS:
        raise SystemExit(f"La clave {primary} solo tiene {len(certifiers)} certificaciones de claves maestras")
    why = f"certificada por {len(certifiers)} claves maestras: " + ", ".join(sorted(c[-16:] for c in certifiers))
    _signer_cache[primary] = why
    return why


def verify_sig(home: Path, data: Path, sig: Path, trusted: set[str]) -> str:
    r = gpg(home, "--status-fd", "1", "--verify", msys(sig), msys(data))
    fprs = re.findall(r"\[GNUPG:\] VALIDSIG ([0-9A-F]{40})", r.stdout)
    if r.returncode != 0 or not fprs:
        raise SystemExit(f"FIRMA NO VÁLIDA para {data.name}:\n{r.stdout}\n{r.stderr}")
    # VALIDSIG incluye la huella de la subclave y, al final de la línea, la de la clave primaria.
    line = next(l for l in r.stdout.splitlines() if "VALIDSIG" in l)
    primary = line.split()[-1]
    signer_is_trusted(home, primary, trusted)
    return primary


def decompress(b: bytes) -> bytes:
    if b[:4] == b"\x28\xb5\x2f\xfd":
        return zstd.decompress(b)
    if b[:2] == b"\x1f\x8b":
        import gzip
        return gzip.decompress(b)
    return b


def parse_db(raw: bytes) -> dict[str, dict]:
    pkgs: dict[str, dict] = {}
    with tarfile.open(fileobj=io.BytesIO(decompress(raw))) as t:
        for m in t.getmembers():
            if not m.name.endswith("/desc"):
                continue
            text = t.extractfile(m).read().decode("utf-8")
            fields: dict[str, list[str]] = {}
            for block in text.strip().split("\n\n"):
                lines = block.strip().split("\n")
                fields[lines[0].strip("%")] = lines[1:]
            name = fields["NAME"][0]
            pkgs[name] = fields
            for prov in fields.get("PROVIDES", []):
                pkgs.setdefault("provides:" + re.split(r"[<>=]", prov)[0], fields)
    return pkgs


def resolve(pkgs: dict[str, dict], root: str) -> list[dict]:
    seen: dict[str, dict] = {}
    stack = [root]
    while stack:
        name = re.split(r"[<>=]", stack.pop())[0]
        p = pkgs.get(name) or pkgs.get("provides:" + name)
        if p is None:
            raise SystemExit(f"Dependencia no encontrada en la base de datos: {name}")
        n = p["NAME"][0]
        if n in seen:
            continue
        seen[n] = p
        stack.extend(p.get("DEPENDS", []))
    return sorted(seen.values(), key=lambda p: p["NAME"][0])


def fetch_keyring_file(name: str) -> bytes:
    """Descarga un fichero del repositorio de claves fijado y exige su SHA-256 esperado."""
    data = fetch(KEYRING_BASE + name)
    got = sha256(data)
    if got != KEYRING_SHA256[name]:
        raise SystemExit(f"{name}: SHA-256 {got} distinto del esperado {KEYRING_SHA256[name]}")
    return data


def main(dest: str) -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    out = Path(dest).resolve()
    dl = out / "downloads"
    home = out / "gnupg"
    for d in (out, dl, home, out / "bin", out / "licenses"):
        d.mkdir(parents=True, exist_ok=True)

    print("1/4 Claves de MSYS2 (github.com/msys2/MSYS2-keyring)")
    keyring = fetch_keyring_file("msys2.gpg")
    trusted_raw = fetch_keyring_file("msys2-trusted").decode()
    trusted = {l.split(":")[0].strip() for l in trusted_raw.splitlines() if l.strip() and not l.startswith("#")}
    (dl / "msys2.gpg").write_bytes(keyring)
    r = gpg(home, "--import", msys(dl / "msys2.gpg"))
    if r.returncode != 0:
        raise SystemExit(f"No se pudieron importar las claves: {r.stderr}")
    print(f"   {len(trusted)} claves de confianza; msys2.gpg sha256={sha256(keyring)}")

    print("2/4 Base de datos ucrt64 firmada")
    db = fetch(MIRROR + "ucrt64.db")
    (dl / "ucrt64.db").write_bytes(db)
    (dl / "ucrt64.db.sig").write_bytes(fetch(MIRROR + "ucrt64.db.sig"))
    db_signer = verify_sig(home, dl / "ucrt64.db", dl / "ucrt64.db.sig", trusted)
    pkgs = parse_db(db)
    print(f"   firma válida (clave {db_signer}); sha256={sha256(db)}")

    print("3/4 Paquetes (SHA-256 de la base firmada + firma propia)")
    needed = resolve(pkgs, ROOT_PACKAGE)
    record = []
    for p in needed:
        fname, expect = p["FILENAME"][0], p["SHA256SUM"][0]
        data = fetch_cached(dl / fname, MIRROR + fname, expect)
        got = sha256(data)
        if got != expect:
            raise SystemExit(f"SHA-256 DISTINTO en {fname}: {got} != {expect}")
        sig = dl / (fname + ".sig")
        if not sig.exists():
            sig.write_bytes(fetch(MIRROR + fname + ".sig"))
        signer = verify_sig(home, dl / fname, dl / (fname + ".sig"), trusted)
        record.append({"name": p["NAME"][0], "version": p["VERSION"][0], "file": fname, "sha256": got,
                       "signer": signer, "license": p.get("LICENSE", [])})
        print(f"   OK {p['NAME'][0]} {p['VERSION'][0]}")

    print("4/4 Extracción (qemu-img.exe, DLL y licencias)")
    for p in record:
        with tarfile.open(fileobj=io.BytesIO(decompress((dl / p["file"]).read_bytes()))) as t:
            for m in t.getmembers():
                if not m.isfile():
                    continue
                parts = m.name.split("/")
                if len(parts) == 3 and parts[1] == "bin" and (parts[2].lower().endswith(".dll") or parts[2] == "qemu-img.exe"):
                    (out / "bin" / parts[2]).write_bytes(t.extractfile(m).read())
                elif len(parts) >= 4 and parts[1] == "share" and parts[2] == "licenses":
                    target = out / "licenses" / Path(*parts[3:])
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(t.extractfile(m).read())
    exe = out / "bin" / "qemu-img.exe"
    if not exe.exists():
        raise SystemExit("qemu-img.exe no está en los paquetes descargados")
    prov = {
        "root_package": ROOT_PACKAGE,
        "mirror": MIRROR,
        "keyring_source": KEYRING_BASE,
        "keyring_commit": KEYRING_COMMIT,
        "keyring_sha256": sha256(keyring),
        "trusted_fingerprints": sorted(trusted),
        "db_sha256": sha256(db),
        "db_signer": db_signer,
        "signer_trust": {k: v for k, v in _signer_cache.items()},
        "min_master_certifications": MIN_MASTER_CERTIFICATIONS,
        "qemu_img_sha256": sha256(exe.read_bytes()),
        "packages": record,
    }
    (out / "PROVENANCE.json").write_text(json.dumps(prov, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"qemu-img.exe sha256={prov['qemu_img_sha256']}")
    print(f"Procedencia registrada en {out / 'PROVENANCE.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else os.path.join(".tools", "qemu")))
