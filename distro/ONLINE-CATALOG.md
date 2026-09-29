# Contrato do acervo online — versão 1

O servidor existente deverá expor este contrato ou receber um adaptador.
Nenhum servidor de produção foi configurado. O cliente fica desativado enquanto
`JUKEBOX_CATALOG_URL` estiver vazio. Não há relação com o serviço de créditos/Pix.
Para o piloto no R2 e a publicação em lote, veja `R2-PILOT.md` e
`R2-MULTIPLOS-ALBUNS.md`. Para o aplicativo de escritório, veja
`PUBLICADOR-ESCRITORIO.md`.

Em `/dados/jukebox.env`:

```sh
JUKEBOX_CATALOG_URL=https://servidor.exemplo/acervo/index.json
JUKEBOX_CATALOG_INTERVAL=1800
JUKEBOX_DOWNLOAD_KIB=0
```

Intervalo mínimo de 60 segundos; um arquivo por vez. Valor `0` não limita a
velocidade no cliente; valores positivos definem o limite em KiB/s. O servidor
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
IDs e versões são strings. Mude a versão do álbum sempre que um arquivo ou
seus metadados de gênero/artista/título mudarem.
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
Mudanças de gênero/artista/título de um ID existente movem o álbum para a nova
pasta, reaproveitando arquivos íntegros. O estado acompanha a movimentação;
interrupções após renomear são recuperadas na próxima consulta. Arquivos ausentes
ou corrompidos são restaurados. O cliente recusa sobrescrever uma pasta de origem USB
ou de outro ID. Mantenha um único ID por Gênero/Artista/Álbum.

`catalog-state.json` guarda apenas o estado de sincronização. O SQLite operacional
não é baixado nem substituído. Após publicar, o aplicativo indexa as novas faixas
com suas rotinas locais. Arquivos modificados são reindexados e registros de
caminhos confirmados como ausentes são removidos do catálogo. Créditos, Pix e
fila não são alterados por essa limpeza. Uma pasta de mídia indisponível ou
erros de acesso não são interpretados como exclusão de toda a coleção.

## Liberar espaço nesta máquina

No menu do operador (`X`), abra **Armazenamento / álbuns**. A lista mostra
artista, álbum, gênero, tamanho estimado em MiB e origem (ONLINE ou USB).
Use `Q/E` para subir, `W/R` para descer e `O` para abrir a confirmação.
A confirmação começa em **Voltar**; selecione **Remover desta máquina** e
confirme com `O`. Tanto a lista quanto a confirmação têm retorno explícito.
O menu permanece aberto e a reprodução continua durante a operação.

A remoção apaga a pasta local e as cópias antigas/parciais em `.downloads`
identificadas como pertencentes ao mesmo álbum. Pastas sem identificação são
preservadas. O tamanho exibido inclui essas cópias e deduplica hardlinks, mas
é uma estimativa: a liberação real depende do sistema de arquivos. Pastas de
vídeos de fundo não aparecem na lista. Uma pasta que contenha outros álbuns
deve ter esses álbuns internos removidos primeiro.

Álbuns com faixa tocando ou na fila não podem ser removidos. Enquanto a
remoção está em andamento, o player também bloqueia novas compras daquele
álbum. Ao concluir, o catálogo SQLite e o carrossel são atualizados, mantendo
créditos e a fila. A operação usa o mesmo lock da sincronização online e USB.
Se houver uma atualização em andamento, aguarde sua conclusão.

Para álbuns ONLINE, `catalog-exclusions.json` registra uma exclusão **local por
ID**, persistida antes de apagar arquivos. A sincronização não baixa esse ID
novamente, mesmo se gênero, nome ou versão mudar no servidor. Outras máquinas
e os arquivos do R2 não são alterados. Preserve esse arquivo em backups das
configurações; removê-lo reabilita os downloads. Um arquivo de exclusões
inválido interrompe a sincronização, em vez de ignorar as exclusões.

O álbum removido permanece nesta lista com a opção **Restaurar**. Confirmar
libera o ID e solicita a atualização online; sem conexão ele será baixado no
próximo ciclo bem-sucedido. Para um álbum USB, importe novamente pelo pendrive.
Se o álbum tiver sido retirado do índice do servidor, ele não será restaurado
até voltar ao índice. Após restaurar, use **Atualizar lista** para ver seu estado.

Atualize o executável e os dois scripts `storage_manage.py` e `catalog_sync.py`
juntos. Na distro os scripts ficam em `/opt/jukebox`; na execução de teste o
aplicativo encontra os scripts no checkout. Não apague pastas manualmente
para excluir um álbum online, pois o sincronizador repõe arquivos ausentes.

Testes do cliente sem acesso à rede:

```sh
python3 -m unittest discover -s tests -v
```
