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
| Moedeiro | USB via javax.usb (jsr80) + JNA | ponto aberto: marca/modelo do aceitador (ver Pendências) |
| Saída de vídeo | X11, janela embutida do VLC | mesma técnica: janela X11 posicionada sob a UI |

## Diferenças em relação ao perfil modern (main)

| Módulo | `legacy` (fase 1) | `modern` (main hoje) |
|---|---|---|
| Build | musl estático i686, sem glibc | x86-64, Debian 13 |
| Player | libVLC 1.1 via FFI; vídeo em janela X11 | GStreamer + appsink (vídeo composto no Slint) |
| Banco | PostgreSQL 9.1 `jukeboxtvdb` (schema original) | SQLite `/dados/jukebox.db` |
| Catálogo | lido do banco (disco/artista/midia + capa bytea) | scanner de arquivos + fingerprints |
| Grade | 5×2 (10 capas/página, layout Jukebox TV) | 5×2 (unificado neste branch) |
| Sync de acervo | compatível com o canal de atualização atual | R2/HTTPS incremental |

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

## Pendências para fechar a fase 1

1. **Moedeiro**: marca/modelo do aceitador de moedas e como o pulso chega
   (USB-HID teclado, USB cru, serial). Define `evdev` vs `rusb` vs serial.
2. **Pix**: confirmar se o backend do sistema Java atual é o PixLogic (o
   cliente Rust deste repo já fala com ele) ou outro serviço.
3. **Jar em produção**: o que roda hoje tem pix e atualização online e não é
   o v28/2013 analisado; enviar para mapear o canal de atualização.
4. **Build musl**: adicionar job de CI cruzado `i686-unknown-linux-musl` com
   player libVLC stubado em teste (fakesink equivalente).
5. **Validação de campo**: RAM/boot/latência de navegação na máquina real
   (P4, 1 GB) com o catálogo de 3.497 discos carregado do PostgreSQL.
