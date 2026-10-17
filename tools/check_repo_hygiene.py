#!/usr/bin/env python3
"""Auditoría de higiene de los ficheros versionados (la ejecuta la CI y se puede ejecutar en local).

Uso: python -I tools/check_repo_hygiene.py

Comprueba sobre `git ls-files` (más los ficheros nuevos aún no versionados que Git no ignora):
  1. Puntuación: ningún carácter U+2014 en ficheros de texto.
  2. Rutas personales: perfiles de usuario de Windows, macOS o Linux con nombre concreto.
  3. Correos: solo se admiten direcciones `noreply` de GitHub y dominios de ejemplo.
  4. Credenciales: claves privadas y formatos conocidos de tokens.
  5. Material excluido: evidencias decompiladas y datos extraídos de la herramienta de referencia,
     imágenes de disco, binarios compilados, bases de datos de trabajo y registros.
  6. Binarios: solo los de la lista permitida, y cada uno debe regenerarse byte a byte con su
     herramienta (recursos originales y reproducibles).
Sale con código 1 si encuentra algún problema; nunca modifica ficheros.
"""
from __future__ import annotations

import hashlib
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

FORBIDDEN_CHAR = chr(0x2014)

# Perfiles de usuario con un nombre concreto (se admiten los genéricos de Windows).
PERSONAL_PATH = re.compile(
    r"(?i)\b[a-z]:[\\/]{1,2}users[\\/]{1,2}(?!public\b|default\b|all users\b)[^\\/\s\"'`<>|*?]+[\\/]"
    r"|/home/(?!runner\b)[a-z0-9._-]+/"
    r"|/Users/(?!Shared\b)[A-Za-z0-9._-]+/"
)
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
ALLOWED_EMAIL = re.compile(r"(?i)(@users\.noreply\.github\.com|@example\.(com|org|net))$")
SECRETS = [
    ("clave privada", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("token de GitHub", re.compile(r"\b(gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b")),
    ("clave de acceso de AWS", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("token de Slack", re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b")),
    ("clave de API genérica", re.compile(r"(?i)\b(api[_-]?key|secret|password|passwd)\s*[:=]\s*['\"][^'\"\s]{12,}['\"]")),
]
EXCLUDED_PATH = [
    ("evidencia decompilada", re.compile(r"(^|/)evidencias-decompiladas/|\.ghidra\.c$")),
    ("datos extraídos del binario de referencia", re.compile(r"^docs/investigacion/dmde-fase1/datos/")),
    ("imagen de disco", re.compile(r"(?i)\.(img|ima|raw|dd|iso|vhd|vhdx|vmdk|e01|qcow2?|vdi|dmg|\d{3})$")),
    ("binario compilado", re.compile(r"(?i)\.(exe|dll|pdb|lib|obj|so|dylib|nupkg)$")),
    ("base de datos de trabajo", re.compile(r"(?i)\.(mrdb|mrdb-wal|mrdb-shm|sqlite|db)$")),
    ("registro", re.compile(r"(?i)\.log$")),
    ("herramienta local", re.compile(r"^\.tools/|(^|/)(bin|obj|target|artifacts)/")),
]
# Binarios permitidos: ruta → orden que lo regenera (debe producir exactamente el mismo contenido).
ALLOWED_BINARIES = {
    "app/src/MemRescue/Assets/MemRescue.ico": [sys.executable, "-I", "tools/make_icon.py", "{out}"],
}


def tracked_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT, capture_output=True, check=True,
    ).stdout
    return sorted({p for p in out.decode("utf-8").split("\0") if p and (ROOT / p).is_file()})


def is_binary(data: bytes) -> bool:
    if b"\0" in data[:8192]:
        return True
    try:
        data.decode("utf-8")
        return False
    except UnicodeDecodeError:
        return True


def main() -> int:
    problems: list[str] = []
    files = tracked_files()
    for rel in files:
        for what, rx in EXCLUDED_PATH:
            if rx.search(rel):
                problems.append(f"{rel}: {what} (no debe versionarse)")
        data = (ROOT / rel).read_bytes()
        if is_binary(data):
            if rel not in ALLOWED_BINARIES:
                problems.append(f"{rel}: binario no permitido (añádalo a ALLOWED_BINARIES con su generador)")
            continue
        text = data.decode("utf-8")
        for n, line in enumerate(text.splitlines(), 1):
            if FORBIDDEN_CHAR in line:
                problems.append(f"{rel}:{n}: carácter U+2014 (use dos puntos, coma, punto o paréntesis)")
            m = PERSONAL_PATH.search(line)
            if m:
                problems.append(f"{rel}:{n}: ruta personal «{m.group(0)}»")
            for e in EMAIL.findall(line):
                if not ALLOWED_EMAIL.search(e):
                    problems.append(f"{rel}:{n}: dirección de correo «{e}»")
            for what, rx in SECRETS:
                if rx.search(line):
                    problems.append(f"{rel}:{n}: posible {what}")

    for rel, cmd in ALLOWED_BINARIES.items():
        path = ROOT / rel
        if not path.is_file():
            problems.append(f"{rel}: falta el binario permitido")
            continue
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "regen")
            r = subprocess.run([c.replace("{out}", out) for c in cmd], cwd=ROOT, capture_output=True, text=True)
            if r.returncode != 0 or not os.path.isfile(out):
                problems.append(f"{rel}: no se pudo regenerar ({r.stderr.strip()[:200]})")
                continue
            a = hashlib.sha256(path.read_bytes()).hexdigest()
            b = hashlib.sha256(Path(out).read_bytes()).hexdigest()
            if a != b:
                problems.append(f"{rel}: no coincide con su generador ({a[:16]} != {b[:16]})")

    for p in problems:
        print(f"ERROR {p}")
    print(f"{len(files)} ficheros revisados, {len(problems)} problemas")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
