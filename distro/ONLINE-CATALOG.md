# Contrato do acervo online — versão 1

O servidor existente deverá expor este contrato ou receber um adaptador.
Nenhum servidor de produção foi configurado. O cliente fica desativado enquanto
`JUKEBOX_CATALOG_URL` estiver vazio. Não há relação com o serviço de créditos/Pix.

Em `/dados/jukebox.env`:

```sh
JUKEBOX_CATALOG_URL=https://servidor.exemplo/acervo/index.json
JUKEBOX_CATALOG_INTERVAL=1800
JUKEBOX_DOWNLOAD_KIB=512
```

Intervalo mínimo de 60 segundos; um arquivo por vez, limite em KiB/s. O servidor
pode usar URLs assinadas nos manifestos. HTTPS é obrigatório, inclusive em
redirecionamentos. Nenhuma credencial é embutida no cliente. Não há autenticação
por máquina implementada neste protocolo inicial.

Índice (limite 8 MiB; detalhes ficam separados por álbum):

```json
{
  "schema": 1,
  "version": "2026-09-27",
  "albums": [
    {"id": "album-0001", "version": "1", "manifest_url": "https://servidor.exemplo/acervo/album-0001.json"}
  ]
}
```

Manifesto do álbum:

```json
{
  "schema": 1,
  "id": "album-0001",
  "version": "1",
  "genre": "Rock",
  "artist": "Artista",
  "title": "Nome do álbum",
  "files": [
    {"path": "01 - Música.mp3", "url": "https://servidor.exemplo/arquivos/musica.mp3", "size": 1234567, "sha256": "SUBSTITUIR_POR_SHA256_HEXADECIMAL_DE_64_CARACTERES"},
    {"path": "cover.jpg", "url": "https://servidor.exemplo/arquivos/capa.jpg", "size": 12345, "sha256": "SUBSTITUIR_POR_SHA256_HEXADECIMAL_DE_64_CARACTERES"}
  ]
}
```

Os exemplos são ilustrativos; tamanhos e hashes precisam corresponder aos arquivos.
IDs e versões são strings. Mude a versão do álbum sempre que um arquivo mudar.
Não altere o conteúdo de uma versão já publicada. Prefira capas reduzidas no
servidor (256×256 ou próximas disso).

O índice é consultado a cada ciclo, mas os manifestos dos álbuns inalterados não.
Downloads parciais ficam em `/dados/.downloads`, fora da varredura do player.
O servidor deve aceitar Range/206 para retomada; se responder 200, o arquivo é
reiniciado. Tamanho e SHA-256 são conferidos antes da publicação.

No Linux, `renameat2(RENAME_EXCHANGE)` troca um álbum completo atomicamente.
Use ext4 e mantenha downloads/acervo na mesma partição. O cliente reserva pelo
menos 128 MiB de espaço livre. O álbum anterior fica em `.downloads` para
recuperação; a limpeza automática de versões antigas ainda não está implementada.
Acompanhe o espaço livre e faça a limpeza em manutenção.

Arquivos omitidos são preservados. IDs ausentes do índice não provocam exclusão.
Mudanças de gênero/artista/título de um ID existente são recusadas até existir
uma migração explícita. O cliente recusa sobrescrever uma pasta de origem USB
ou de outro ID. Mantenha um único ID por Gênero/Artista/Álbum.

`catalog-state.json` guarda apenas o estado de sincronização. O SQLite operacional
não é baixado nem substituído. Após publicar, o aplicativo indexa as novas faixas
com suas rotinas locais. A atualização de metadados ID3 de faixas já indexadas
não é reconciliada nesta versão; use nomes/pastas estáveis.

Testes do cliente sem acesso à rede:

```sh
python3 -m unittest discover -s tests -v
```
