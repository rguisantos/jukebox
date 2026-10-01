# Perfil Legacy — Jukebox TV (base antiga)

Este branch adapta o aplicativo Rust para rodar **nas máquinas Jukebox TV em
campo**, como substituto direto do programa Java, antes da troca do sistema
operacional. A estratégia é trocar o programa primeiro e o sistema depois:

```
HOJE:    Ubuntu 11.04 (i686) + Java 6 + VLC 1.1 + PostgreSQL 9.1   (10 anos em campo)
FASE 1:  Ubuntu 11.04 (i686) + RUST    + VLC 1.1 + PostgreSQL 9.1  ← este perfil
FASE 2:  validação em produção (1 máquina); rollback = jukebox.sh do Java
FASE 3:  Debian 13 + RUST + GStreamer + PostgreSQL 17              ← perfil modern
```

O contrato entre as fases é o banco: a fase 1 lê e escreve o **mesmo
`jukeboxtvdb`** do sistema Java (créditos, fila, catálogo, relatórios), então a
migração não duplica dados e o rollback é imediato — o script do Java continua
instalado e o estado persiste nas tabelas originais.

## Fatos da base em campo (extraídos dos hs_err e scripts originais)

| Componente | Valor | Consequência para o build |
|---|---|---|
| SO | Ubuntu 11.04 Natty, kernel 2.6.38, i686 | glibc 2.13 → alvo `i686-unknown-linux-musl` (estático) |
| CPU | Pentium 4 3.00GHz (HT, SSE3, sem x86-64) | sem SIMD moderno; renderer por software |
| RAM | ~991 MB + 2 GB swap | orçamento apertado; a JVM usava heap de 512 MB–2 GB |
| Player | VLC 1.1.x (via vlcj 2.1.0) | GStreamer local é 0.10 → backend de vídeo por **libVLC (FFI)** |
| Banco | PostgreSQL 9.1 local (JDBC 901) | protocolo v3 → `tokio-postgres` compatível |
| Moedeiro | USB HID (teclado comum); pulso = tecla Z | `arcade-key-pressed` nativo + `legacy_keys.rs` |
| Saída de vídeo | X11, janela embutida do VLC | mesma técnica: janela X11 posicionada sob a UI |

## Diferenças em relação ao perfil modern (main)

| Módulo | `legacy` (fase 1) | `modern` (main hoje) |
|---|---|---|
| Build | musl estático i686, sem glibc | x86-64, Debian 13 |
| Player | libVLC 1.1 via FFI; vídeo em janela X11 | GStreamer + appsink (vídeo composto no Slint) |
| Banco | PostgreSQL 9.1 `jukeboxtvdb` (schema original) | SQLite `/dados/jukebox.db` |
| Catálogo | **`db/legacy_pg.rs`** — lido do banco (disco/artista/midia + capa bytea) | scanner de arquivos + fingerprints |
| Grade | 5×2 (10 capas/página, layout Jukebox TV) | 5×2 (unificado neste branch) |
| Sync de acervo | sincronização HTTPS do próprio app Rust | R2/HTTPS incremental |

O adaptador `db/legacy_pg.rs` (feature `legacy-pg`) está pronto e testado:
leitura do catálogo com estilos habilitados, capas `bytea` sob demanda,
linha `sistema` completa (teclas, créditos, incentivos, brinde, volume,
grade), entradas de crédito em transação `FOR UPDATE` com histórico em
`registro_creditos`, débito + reserva de fila atômicos em `filamidia`,
histórico de execuções em `registro_musicas` e zeroing de caixa. Os tipos
originais são respeitados (créditos `double precision`, `filamidia.midia`
`bigint`, ids por `max(id)+1` sem sequências) e o caminho dos arquivos é
resolvido pela convenção da árvore
`<raiz>/<estilo>/<artista>/<disco>/<nome>.<ext>`.

**Pix e atualização online não existem no sistema Java em campo** — são
recursos que o app Rust traz como ganho novo na fase 1 (PixLogic já
implementado; sincronização de acervo do próprio repo). Não há canal
legado a migrar.

A grade 5×2 e a navegação por linha (`GRID_COLUMNS` em `state/models.rs`) já
foram unificadas neste branch: W/Q saltam uma linha inteira (5 capas), E/R
movem uma capa, e o carrossel instancia apenas as 10 capas da página visível.

## Schema PostgreSQL usado (inspeção física do cluster 9.1)

Contagens da base em campo: midia 55.230, registro_creditos 43.236,
registro_musicas 18.949, disco 3.497 (capas em `bytea`), artista 1.586,
estilo 40, equalizer 18, tipo_disco 3, tipo_midia 2, gravadora 2, sistema 1.

- `artista(id, nome, comparetonome, estilo, timestamp, aleatorio)`
- `disco(id, nome, comparetonome, artista, tipo_disco, capa bytea, extensao_capa, timestamp, aleatorio, lancamento, gravadora)`
- `midia(id, nome, comparetonome, disco, artista, tipo_midia, extensao_conteudo, posicao, timestamp, aleatorio, codigoisrc)`
- `estilo(id, nome, aleatorio, comparetonome, habilita)`
- `filamidia(id, midia, datahora)` — fila persistida
- `registro_creditos(creditos float8, datahora, datahorareset, id)`
- `registro_musicas(midia, datahora, exportado)` — histórico p/ relatórios
- `sistema(...)` — 70 colunas de configuração (teclas, preços, propaganda,
  brinde, equalizer, autoredução de volume, etc.)

A camada de acesso a essas tabelas (já modelada no pacote `jukebox-rs` da
análise anterior) será portada para `src/db/legacy_pg.rs` atrás da mesma
interface de comandos usada pelo `storage/service.rs` atual.

## Entrada e pagamento (resolvido em campo)

- **Interface de botões + moedeiro = teclado USB comum (HID)**. Os pulsos
  de crédito chegam como a tecla configurada (tipicamente **Z**) e os
  botões como as demais teclas. O hardlock embutido na placa servia apenas
  à verificação de licença do Java — o Rust não lê nem precisa dele.
  Nenhum driver customizado: o kernel entrega os eventos pelo X11 e o app
  os recebe em `arcade-key-pressed`; `z` dispara `Action::AddCredit` em
  qualquer tela.
- As teclas são configuráveis por máquina nas colunas `sistema.codtecla*`
  (códigos AWT `java.awt.event.KeyEvent`). A camada `src/legacy_keys.rs`
  traduz o código AWT de cada máquina para a tecla canônica do app
  (e/r/q/w/i/o/u/p/z/a/l) — a placa não precisa ser reprogramada.
  Pendência menor: `codteclamaisvolume`/`codteclamenosvolume` aguardam o
  overlay de volume aceitar códigos dedicados de +/− (hoje W/Q).
- **Pix = PixLogic** (firmware ESP8266 v2.2). O cliente Rust deste repo
  (`finance/pixlogic.rs`) já fala o protocolo: consulta
  `/api/machine/{uuid}/credit` a cada 3 s, aplica o crédito antes de
  confirmar em `/confirm` com `X-Device-Token`, e retoma confirmações
  pendentes após reinício. A fase 1 reutiliza o módulo como está; a única
  adaptação é gravar o crédito na tabela legacy (`registro_creditos` /
  contador do `sistema`) em vez do SQLite.

## Pendências para fechar a fase 1

1. **Wiring no main.rs**: ligar o serviço `legacy_pg::client::LegacyDb` ao
   fluxo do app (catálogo → carrossel, entrada_moeda → Action::AddCredit,
   enqueue/dequeue → player, sistema → legacy_keys e preços) atrás da
   feature `legacy-pg`.
2. **Player libVLC**: backend alternativo ao GStreamer para o VLC 1.1 da
   base (vídeo em janela X11 embutida, como o vlcj original).
3. **Build musl**: job de CI cruzado `i686-unknown-linux-musl` (o runner
   atual já valida a feature `legacy-pg` em x86-64).
4. **Validação de campo**: RAM/boot/latência de navegação na máquina real
   (P4, 1 GB) com o catálogo de 3.497 discos carregado do PostgreSQL, e
   conferência das premissas semânticas com o operador (incentivos por
   cédula, preservação do contador do brinde no zeroing).
