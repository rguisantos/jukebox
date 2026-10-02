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
grade, janela de lançamentos), entradas de crédito em transação `FOR
UPDATE` com histórico em `registro_creditos`, débito + reserva de fila
atômicos em `filamidia`, histórico de execuções em `registro_musicas`,
zeroing de caixa, bloqueio de estilos, números do operador e snapshot da
fila. Os tipos originais são respeitados (créditos `double precision`,
`filamidia.midia` `bigint`, ids por `max(id)+1` sem sequências) e o caminho
dos arquivos é resolvido pela convenção da árvore
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

A camada de acesso a essas tabelas vive em `src/db/legacy_pg.rs` atrás da
mesma interface de comandos usada pelo `storage/service.rs` atual.

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

## Wiring do perfil (implementado)

O perfil é selecionado em tempo de execução: binário construído com a
feature `legacy-pg` **e** `JUKEBOX_PROFILE=legacy` no ambiente (o launcher
da base antiga define). Sem isso, o app sobe exatamente como hoje
(SQLite/GStreamer) — o perfil modern não mudou de comportamento.

Com o perfil legacy ativo:

- **Boot**: conecta ao `jukeboxtvdb` antes da janela (sem banco não há app,
  como no Java), lê a linha `sistema` → saldo, volume (clamp por
  `maxvolume`, padrão 70 se vazio), janela de lançamentos
  (`diaslancamento`) e teclas da máquina (`codtecla*` → mapa em
  `LEGACY_KEYMAP`).
- **Persistência**: `storage::legacy_service.rs` atende o mesmo protocolo
  `DbCommand`/`DbEvent` do serviço SQLite — o `main.rs` não tem fluxo
  próprio. Moeda/pix credita em transação `FOR UPDATE` + histórico;
  catálogo e gêneros são publicados no boot (no lugar do scanner);
  bloqueio de gênero grava `estilo.habilita`; zeroing abre novo período;
  `resetacreditos` zera só o saldo. O que não existe no schema original
  (pacotes, preço configurável) responde com toast informativo — o preço
  por música é a constante do sistema original: **1 crédito**.
- **Capas**: `storage::legacy_covers.rs` serve `disco.capa` (bytea) para o
  pipeline existente de capas no *miss* do cache — decodificação sob
  demanda (256 px), **sem** cache em disco (o JBC em RGB custaria ~700 MiB
  no disco IDE da base).
- **Player (libVLC — fase 2)**: `media/legacy_vlc.rs` assume o canal
  `PlayerCommand`/`PlayerEvent` e reproduz com o **VLC 1.1 da própria
  máquina**, carregado por soname (`libvlc.so.5`, ABI estável até o 3.x)
  via `libloading` — o binário não linka VLC/X11 em tempo de build.
  Vídeo embutido numa **janela X11 filha** da janela Slint
  (`media/legacy_x11.rs`), a técnica do vlcj original: o VLC renderiza
  direto (Xv/X11), zero cópia de pixels pela CPU no Pentium 4. O event
  loop sincroniza a geometria do overlay a cada tick (`overlay_sync`);
  tela cheia = janela toda. Cada execução grava `registro_musicas` e
  conta o brinde no **início** (`contbrinde`; ao bater
  `minmusicasbrinde`, credita `premiocredbrinde` — premissa portada do
  jukebox-rs). Fila vazia + modo aleatório liberado (`modoaleatorio` ou
  `modofesta`, saldo ≥ `mininicioaleatorio`) sorteia uma `midia.aleatorio`
  **sem débito** e sem gravar na fila (música da casa). Sem libVLC no
  sistema, degrada para o modo de validação (`legacy_player::run`):
  débito e fila reais, sem mídia.
- **Teclado**: o mapa `runtime_keymap()` aplica as `codtecla*` da máquina
  a cada tecla, preservando o sufixo `_long`. Na colisão de textos vence a
  navegação (nunca há crédito fantasma). A propriedade `pass-through-enter`
  do FocusScope entrega o Enter como `"enter"` literal no perfil legacy,
  eliminando a colisão com as teclas I/O físicas do mapeamento padrão.
- **Fora do perfil**: scanner de arquivos, sincronização USB/online,
  gerenciamento de armazenamento e o monitor de disco ficam desligados (o
  acervo é do importador original); `PowerOff` usa `poweroff` direto
  (base sem systemd/sudo).

## Pendências para fechar a fase 1

1. ~~Wiring no main.rs~~ (implementado; validação de compilação completa a
   cargo do CI — o sandbox não tem GStreamer/Slint dev).
2. ~~Player libVLC~~ (implementado: `legacy_vlc.rs` + `legacy_x11.rs`,
   28 testes no harness do sandbox; verificação em campo pendente).
3. **Build para a base antiga**: job de CI cruzado (o runner atual valida
   a feature `legacy-pg` em x86-64). Decisão de estratégia pendente de
   bancada — ver "Build para a base antiga" abaixo.
4. **Validação de campo**: RAM/boot/latência de navegação na máquina real
   (P4, 1 GB) com o catálogo de 3.497 discos carregado do PostgreSQL,
   conferência das premissas semânticas com o operador (incentivos por
   cédula, brinde creditado ao bater o contador, registro de execução no
   início) e do overlay X11 (posição/tela cheia/ocultação em clipe).
5. **Pendências menores**: `codteclamaisvolume`/`codteclamenosvolume` no
   overlay de volume; idempotência PixLogic persistida (hoje vale por
   sessão — o schema original não tem tabela para a pendência);
   autoredução de volume (`autoreducaovolume`/`horas`/`teto`) e
   propaganda/clipes de fundo (`clipesobremusica`), que ficam de fora
   até a validação de campo das premissas centrais.

## Build para a base antiga (decisão de bancada)

O alvo é Ubuntu 11.04 i686, **glibc 2.13** — e o binário precisa
`dlopen`ar o VLC 1.1 e o X11 **do sistema** (glibc). As estratégias:

1. **musl estático** (`i686-unknown-linux-musl`): binário indepentente de
   libc, mas `dlopen` de DSOs ligados à glibc a partir de um binário
   musl estático é cenário sabidamente quebrado (duas libcs num
   processo). **Só funciona se** o plano B abaixo for inviável e o
   fallback "processo VLC filho" (item 3) for aceito.
2. **glibc dinâmico com piso 2.13** (preferível): cross-build com
   `zig cc -target i686-linux-gnu.2.13` (cargo-zigbuild) ou um sysroot
   do Natty (`old-releases.ubuntu.com`) + clang/lld. O binário fala a
   glibc da máquina e `dlopen` funciona nativamente — igual ao Java
   original. Ponto em aberto: o piso oficial do rustc é glibc 2.17; se
   o link reclamar de símbolos mais novos (`__cxa_thread_atexit_impl`),
   um shim fraco resolve (perder dtors de thread-local é aceitável
   num jukebox de threads longas).
3. **Fallback sem FFI**: subir o `vlc` da máquina como **processo filho**
   (`vlc --drawable-xid <XID> --play-and-exit <arquivo>`) — isola as
   libcs por processo, ao custo de ~200 ms por faixa e sem volume em
   tempo real.

A escolha se decide na bancada com o binário real; o código do player
já é agnóstico (dlopen por soname).

## Premissas semânticas do player (conferir com o operador)

- `registro_musicas` é gravado no **início** da execução (o crédito já
  foi consumido no enqueue — arquivo quebrado conta como tocada, como
  no original).
- Brinde: ao bater `minmusicasbrinde` execuções, `premiocredbrinde`
  créditos são creditados automaticamente e o contador zera
  (`premiar_brinde`), com `txtbrinde` no toast.
- Modo aleatório: `modofesta` sempre; `modoaleatorio` exige saldo ≥
  `mininicioaleatorio`; a faixa sorteada (`midia.aleatorio`, estilos
  habilitados; sem clipes quando `aleatoriovideo = false`) **não** vai
  para `filamidia` — toca direto, sem débito.
- Skip: a faixa atual conta como tocada (histórico no início) e o
  player avança; fila parada descarta a cabeça sem registrar.
