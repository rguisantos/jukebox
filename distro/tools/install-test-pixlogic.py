#!/usr/bin/env python3
"""Instala o perfil de teste a partir do .ino local sem publicar o token."""

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import tempfile


PROFILE = Path(__file__).resolve().parents[1] / "test-machines/pixlogic-b4450dba.env"
CONFIG = re.compile(r'const\s+char\s+EMBEDDED_CONFIG\[\]\s+PROGMEM\s*=\s*"((?:\\.|[^"\\])*)"\s*;')
KEYS = ("JUKEBOX_PIXLOGIC_API", "JUKEBOX_PIXLOGIC_UUID", "JUKEBOX_PIXLOGIC_TOKEN")


def install(ino: Path, destination: Path) -> None:
    profile = dict(line.split("=", 1) for line in PROFILE.read_text().splitlines()
                   if line.startswith("JUKEBOX_PIXLOGIC_"))
    match = CONFIG.search(ino.read_text())
    if match is None:
        raise ValueError("EMBEDDED_CONFIG não encontrado no .ino")
    machine = json.loads(json.loads('"' + match.group(1) + '"'))
    token = machine.get("token", "")
    if (machine.get("apiBase") != profile["JUKEBOX_PIXLOGIC_API"]
            or machine.get("uuid") != profile["JUKEBOX_PIXLOGIC_UUID"]
            or not isinstance(token, str) or len(token) != 64):
        raise ValueError("O .ino não corresponde à máquina de teste ou contém token inválido")
    if destination == Path("/dados/jukebox.env") and not os.path.ismount("/dados"):
        raise ValueError("/dados não está montado; configuração não instalada")

    original = destination.read_text()
    stat = destination.stat()
    values = {"JUKEBOX_PIXLOGIC_API": machine["apiBase"],
              "JUKEBOX_PIXLOGIC_UUID": machine["uuid"],
              "JUKEBOX_PIXLOGIC_TOKEN": token}
    lines = [line for line in original.splitlines()
             if not any(re.match(r"\s*#?\s*" + key + r"=", line) for key in KEYS)]
    lines.extend(key + "=" + shlex.quote(values[key]) for key in KEYS)
    fd, tmp = tempfile.mkstemp(prefix=".jukebox.env.", dir=destination.parent)
    try:
        os.fchmod(fd, 0o600)
        if os.geteuid() == 0:
            os.fchown(fd, stat.st_uid, stat.st_gid)
        with os.fdopen(fd, "w") as output:
            output.write("\n".join(lines) + "\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(tmp, destination)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ino", type=Path, help="caminho local do .ino desta máquina")
    parser.add_argument("--destination", type=Path, default=Path("/dados/jukebox.env"))
    args = parser.parse_args()
    try:
        install(args.ino, args.destination)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        parser.exit(1, f"Não foi possível instalar o perfil: {error}\n")
    print("Perfil PixLogic instalado; reinicie a jukebox para ativar a consulta.")
