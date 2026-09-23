# Correção definitiva — Conflito `systemd-sysv` ⇄ `live-config-sysvinit` no build da ISO kiosk (Debian 13 "Trixie")

**Sintoma corrigido:** o build falha sistematicamente no resolvedor de dependências
do APT durante `lb build` / `lb chroot install-packages`, com:

```
E: Error, pkgProblemResolver::Resolve generated breaks, this may be caused by held packages.
E: The following information from --solver 3.0 may provide additional context:
   Unable to satisfy dependencies. Reached two conflicting decisions:
   1. sysvinit-core:amd64 is not selected for install because:
      1. systemd-sysv:amd64 is selected for install
      2. systemd-sysv:amd64 is available in version 257.13-1~deb13u1
      3. sysvinit-core:amd64 Conflicts systemd-sysv
   2. sysvinit-core:amd64 is selected for install because:
      1. live-config-sysvinit:amd64=11.0.5 is selected for install
      2. live-config-sysvinit:amd64 Depends sysvinit-core | sysvinit (< 2.88dsf-44)
```

---

## 1. Diagnóstico técnico (verificado no repositório e reproduzido empiricamente)

### 1.1 A cadeia de dependências real do trixie

Metadados reais extraídos do repositório Debian 13 (`live-config 11.0.5`,
`systemd 257.13-1~deb13u1`, `sysvinit 3.14-4`):

| Pacote | Relação relevante |
|---|---|
| `live-config` | `Depends: live-config-systemd \| live-config-backend` |
| `live-config-systemd` | `Provides: live-config-backend` · `Depends: systemd` |
| `live-config-sysvinit` | `Provides: live-config-backend` · **`Depends: sysvinit-core \| sysvinit (<< 2.88dsf-44)`** |
| `sysvinit-core` | **`Conflicts: systemd-sysv`** |
| `systemd-sysv` | `Priority: important` — já presente no sistema base instalado pelo `debootstrap`; `Conflicts: sysvinit-core, initscripts, insserv, startpar, sysv-rc, file-rc, systemd-shim, orphan-sysvinit-scripts…` |

No trixie **não existe** mais o pacote binário `sysvinit`, e a alternativa
`sysvinit (<< 2.88dsf-44)` do `live-config-sysvinit` é insatisfazível — ou seja:
instalar `live-config-sysvinit` **sempre** arrasta `sysvinit-core`, que **sempre**
conflita com o `systemd-sysv` (exigido pelo autologin do TTY1 e pelo watchdog do
kiosk). A única saída correta é nunca deixar o `live-config-sysvinit` entrar na
transação.

### 1.2 Por que o build quebra

O `live-config` depende de `live-config-systemd | live-config-backend` — a segunda
alternativa é um **pacote virtual** com dois provedores. O novo resolvedor do APT 3.0
do trixie (**solver 3.0**, confirmado na própria mensagem de erro acima) pode, durante
a resolução de transações grandes (o `apt-get install` único do
`lb chroot install-packages` carrega o manifesto inteiro do kiosk), satisfazer a
dependência pela alternativa virtual e **escolher `live-config-sysvinit` como
provedor** — gerando a decisão contraditória "instalar `sysvinit-core`" +
"manter `systemd-sysv`" que o resolvedor não consegue desfazer.

Em transações pequenas e isoladas o APT normalmente escolhe a primeira alternativa
concreta (`live-config-systemd`) — por isso o problema aparece **no install pass do
build** e não numa instalação manual simples.

### 1.3 Por que as tentativas anteriores falharam

- **`exclude.list.chroot` com entradas negativas:** entradas negativas em listas só
  removem o pacote do **conjunto pedido** na linha de comando do `apt-get`. Elas
  **não bloqueiam** pacotes que entram por **dependência** de outros pacotes — e o
  `live-config-sysvinit` entra exatamente como provedor de uma dependência virtual.
  Além disso: o apt-get só entende remoção com hífen **final** (`pacote-`); hífen
  **inicial** (`-pacote`) é interpretado como opção de linha de comando e aborta o
  `apt-get` com erro.
- **`config/archives/*.pref` anterior:** pinagem é de fato o mecanismo certo (é o que
  este kit usa), mas ela precisa (a) da sintaxe exata `Pin: release *` +
  `Pin-Priority: -1` para **todas** as versões/origens, (b) cobrir o pacote certo
  (`live-config-sysvinit` — o gatilho — e não só o `sysvinit-core`, a vítima), e
  (c) estar no local que o live-build copia para o chroot **antes** dos passes de
  instalação (`config/archives/*.pref.chroot` é copiado pelo
  `lb chroot_prep … mode-archives-chroot`, que roda antes de qualquer `apt-get`
  do estágio chroot). A ausência de qualquer um desses três pontos torna a pinagem
  inócua — ela simplesmente nunca chega ao resolvedor no momento da transação.

### 1.4 Evidência empírica (reproduzida em Debian 13 limpo)

Os testes abaixo foram executados num Debian trixie com o repositório oficial
(comandos com `apt-get --simulate`):

1. **Sem pin**, transação que inclui `live-config-sysvinit` → reproduz
   **exatamente** o erro `pkgProblemResolver::Resolve generated breaks` (texto na
   seção "Sintoma").
2. **Com o pin deste kit** → `apt-get install live-config-sysvinit` responde
   `Package 'live-config-sysvinit' has no installation candidate` (bloqueado: o
   APT não pode mais selecioná-lo, nem como provedor do virtual).
3. **Com o pin** → `apt-get install live-config` resolve normalmente via
   `live-config-systemd` (11.0.5), exit 0.
4. **Com o pin + entradas de remoção** (`live-config-sysvinit- sysvinit-core- …` na
   mesma transação, como o live-build monta) → apenas avisos inofensivos
   `is not installed, so not removed` (não-fatais), `live-config-systemd` no
   conjunto de instalação, exit 0.

Ou seja: as três camadas abaixo não são teoria — cada interação foi validada.

---

## 2. Estrutura do kit

```
distro/                                        # raiz do build (lb build roda aqui)
├── README-SYSVINIT-FIX.md                     # este arquivo
├── tools/
│   └── audit-sysvinit.sh                      # auditoria dos manifestos (executável)
└── config/
    ├── archives/
    │   └── 00-block-sysvinit.pref.chroot      # CAMADA 1 — pinagem APT
    ├── package-lists/
    │   ├── live.list.chroot                   # CAMADA 2a — backend concreto (commitada; ver §2.1)
    │   └── 99-remove-sysvinit.list.chroot     # CAMADA 2b — troca ativa
    └── hooks/
        ├── normal/
        │   └── 0091-ensure-live-hooks-run.hook.chroot  # guarda: pré-condição de execução
        └── live/
            ├── 01-setup-kiosk.hook.chroot             # (existente) setup do kiosk — roda antes da purga
            └── 7000-purge-sysvinit-residue.hook.chroot  # CAMADA 3 — purga + verificação
```

Todos os caminhos acima são relativos à **raiz do build** (`distro/`), espelhando
exatamente a árvore que o `lb config` espera.

### 2.1 Variação aplicada neste repositório (jukebox) — `live.list.chroot` commitada

Neste repo, a Camada 2a usa o **nome canônico** `config/package-lists/live.list.chroot`
(em vez de um `00-init-systemd.list.chroot`) por um motivo específico do histórico do
projeto: **o vetor do envenenamento sistemático era exatamente esse arquivo, em estado
local obsoleto.**

A cadeia do problema (verificada no código do live-build `1:20250505+deb13u1` e no
`auto/clean` deste repo):

1. o `lb config` **só gera** `live.list.chroot` se ele **não existir** (guarda
   `if [ ! -e ... ]` em `/usr/lib/live/build/config`);
2. o `lb clean` / `auto/clean` **nunca** remove `config/package-lists/`;
3. o `.gitignore` antigo escondia o arquivo (lista gerada) — logo, um `live.list.chroot`
   da era bookworm/sysvinit (com `live-config-sysvinit` + `sysvinit-core`) sobrevivia a
   **todos** os rebuilds e injetava o backend errado em toda transação do APT no trixie.

Commitando a versão correta (backend systemd), o build fica determinístico: fresh
clone produz a mesma árvore, `git status` denuncia qualquer alteração local, e o
`lb config` respeita o arquivo versionado. Junto disso, o `auto/config` agora declara
`--initsystem systemd` explicitamente (documenta a intenção do kiosk e protege contra
mudança de default em versões futuras do live-build) e o `.gitignore` ganhou as
exceções de versionamento para os arquivos do kit.

---

## 3. As três camadas de defesa

### Camada 1 — Bloqueio na raiz, ANTES de qualquer instalação (pinagem APT)

`config/archives/00-block-sysvinit.pref.chroot` instala `Pin-Priority: -1` para:

- `live-config-sysvinit` (o **gatilho** do conflito — provedor errado do virtual);
- `sysvinit-core`, `initscripts`, `insserv`, `startpar`, `sysv-rc` (a cadeia legada);
- `file-rc`, `systemd-shim`, `orphan-sysvinit-scripts` (hardening opcional —
  alternativas legadas que também conflitam com `systemd-sysv` no trixie).

Com prioridade `-1` esses pacotes **não têm candidato**: o resolvedor não pode
selecioná-los nem como pedido explícito, nem como provedor do
`live-config-backend`, nem por `Recommends`. A dependência do `live-config` passa a
ser satisfeita pela alternativa concreta `live-config-systemd`.

**Quando o bloqueio entra em ação (verificado no código do
live-build `1:20250505+deb13u1`):** o `lb chroot` executa primeiro
`lb chroot_prep install all mode-archives-chroot`, que copia o `.pref.chroot` para
`chroot/etc/apt/preferences.d/` **antes** do `dist-upgrade` interno e dos passes
`install`/`live` do `chroot_install-packages`. O estágio bootstrap (debootstrap puro)
não usa APT e não instala `live-config` (Priority: optional) — não há o que bloquear
ali; o `systemd-sysv` (Priority: important) já entra pela base.

**Nota sobre o sufixo:** `.pref.chroot` existe apenas durante o build — o live-build
o **remove** da imagem final. Para manter o bloqueio também no kiosk em produção
(hardening: impedir que alguém instale sysvinit e quebre o boot), renomeie para
`00-block-sysvinit.pref` (sem sufixo) e ele persistirá na ISO.

**Não bloquear `sysvinit-utils`:** não conflita com `systemd-sysv` e faz parte da
base normal do sistema (`killall5`, `pidof` etc.).

### Camada 2 — Manifestos revisados: backend concreto + troca na mesma transação

- **`00-init-systemd.list.chroot`** — instala `systemd`, `systemd-sysv`,
  `live-boot`, `live-config` e **`live-config-systemd` explicitamente**. Com o
  backend certo já presente na transação, a dependência do `live-config` fica
  satisfeita por um pacote **real** — o resolvedor jamais precisa escolher provedor
  para o virtual. Entradas duplicadas (se `live-boot`/`live-config` já estiverem nos
  seus manifestos) são inofensivas para o APT.
  *Neste repo, esse papel é exercido pelo `live.list.chroot` commitado (§2.1), com o
  mesmo conteúdo que o live-build geraria para os defaults do projeto — e a lista
  `jukebox.list.chroot` já traz `systemd-sysv` explicitamente.*
- **`99-remove-sysvinit.list.chroot`** — entradas com hífen **final**
  (`live-config-sysvinit-`, `sysvinit-core-`, …). Cada passe do live-build executa
  **um único** `apt-get install` com todos os pacotes de todas as listas (via
  `xargs --arg-file`); com essas entradas no conjunto, o APT faz a remoção de
  qualquer resíduo **na mesma transação** — cobre inclusive chroot contaminado por
  cache antigo ou `--bootstrap-include` customizado. Pacote ausente gera apenas o
  aviso não-fatal `is not installed, so not removed`.

### Camada 3 — Hook de limpeza preventiva (purga forçada + verificação final)

`config/hooks/live/7000-purge-sysvinit-residue.hook.chroot` (faixa 7XXX = ganchos do
usuário, conforme `/usr/share/live/build/hooks/README`). O kit inclui também o
guarda `config/hooks/normal/0091-ensure-live-hooks-run.hook.chroot`: o live-build só
executa hooks de `hooks/live/` se existir ao menos um hook em `hooks/normal/` — o
guarda garante essa pré-condição em qualquer árvore, mesmo que os hooks padrão do
`lb config` tenham sido removidos. O hook da Camada 3:

1. **Garante o backend certo primeiro** (`systemd-sysv`, `live-config-systemd`) —
   nunca deixa o sistema sem init;
2. **Purga resíduos** que tenham escapado (ex.: `Recommends` de repositório de
   terceiros) com `apt-get purge -y --allow-remove-essential` em uma única
   transação, e limpa estados `rc` do dpkg;
3. **Verifica e falha rápido**: se ao final `systemd-sysv` não estiver instalado,
   ou qualquer pacote da cadeia sysvinit ainda estiver, ou `/sbin/init` não existir,
   o **build aborta aqui com mensagem clara** — nunca produz ISO não-bootável.

**Precisão técnica sobre o momento do hook:** hooks `config/hooks/live/` rodam
**após** os passes de instalação de pacotes (ordem do estágio chroot:
`chroot_prep` → … → passes `install`/`live` → `includes_after_packages` →
**`chroot_hooks`** → `chroot_hacks` …). O bloqueio *antes* da instalação dos
pacotes do usuário é exatamente o papel da **Camada 1** (pinagem aplicada pelo
`chroot_prep`, antes de qualquer `apt-get`); a Camada 2 executa a troca *dentro* das
transações de instalação; a Camada 3 fecha o sistema após as instalações. Juntas,
cobrem antes/durante/depois.

---

## 4. Como aplicar no seu projeto

**Neste repositório (jukebox):** os arquivos já estão no branch `fix/sysvinit-backend`
— basta fazer merge/checkout do branch e, na máquina onde os builds falhavam,
restaurar o estado versionado dos arquivos locais que estavam fora do git:

```bash
git checkout main && git merge fix/sysvinit-backend   # ou o merge via PR
cd distro/
# remove qualquer resquício local de listas geradas (o live.list.chroot versionado assume):
git checkout -- config/package-lists/ 2>/dev/null || true
git clean -fdn config/package-lists/   # -n = simular; confira antes de rodar sem -n
git clean -fd config/package-lists/    # remove live.list.chroot local obsoleto, se houver
./tools/audit-sysvinit.sh
```

**Em outro projeto live-build:** copie a árvore do kit (config/ e tools/) por cima
da raiz do build, dê permissão de execução aos hooks em `config/hooks/{live,normal}/`
e ao `tools/audit-sysvinit.sh`.

Depois, **audite os seus manifestos existentes** (item 2 do plano — garantir que
nenhum pacote legado esteja listado direta ou indiretamente):

```bash
cd distro/
./tools/audit-sysvinit.sh            # auditoria dos manifestos
./tools/audit-sysvinit.sh --chroot   # se houver um chroot/ de build falho
```

O que o auditor aponta e como tratar:

- `pacote` solto em algum `*.list.chroot` → **remova a linha** (a pinagem da Camada 1
  já bloqueia o pacote; uma entrada explícita faria o `apt-get` falhar com
  "no installation candidate" — falha rápida e desejada, que acusa o manifesto).
- `-pacote` (hífen inicial) → sintaxe **inválida** para o apt-get; apague ou troque
  para `pacote-`.
- Para rastrear dependências **indiretas** (quem puxa o quê) num chroot parcial:

```bash
chroot chroot apt-cache rdepends --important live-config-sysvinit
chroot chroot apt-cache rdepends --important sysvinit-core
# ou, simulando a transação completa com depuração do resolvedor:
chroot chroot apt-get -o Debug::pkgProblemResolver=yes \
    install systemd-sysv live-config live-config-systemd
```

---

## 5. Rebuild limpo (obrigatório)

Cache de chroot antigo pode ressuscitar o estado contaminado — limpe antes:

```bash
cd distro/
lb clean --all        # remove chroot/, caches e artefatos; PRESERVA config/
lb build 2>&1 | tee build.log
```

> `lb clean --purge` apaga também a sua `config/` — **não use** para isso.

Se o seu pipeline constrói em container/VM, descarte a camada/volume de cache junto.

---

## 6. Verificação do resultado

**Durante o build** (build.log):

```bash
grep -n "live-config-sysvinit" build.log
# esperado: no máximo os avisos "is not installed, so not removed"
#           (Camada 2) e o "OK" do hook (Camada 3);
#           NUNCA mais "pkgProblemResolver::Resolve generated breaks"

grep -n "7000-purge-sysvinit-residue" build.log   # hook executado
```

**No chroot, ao final** (build concluído ou interrompido):

```bash
chroot chroot apt-cache policy live-config-sysvinit sysvinit-core
# durante o estágio chroot deve mostrar prioridade -1 / sem candidato
dpkg -l chroot/var/lib/dpkg/status 2>/dev/null || \
grep -A2 "^Package: systemd-sysv$" chroot/var/lib/dpkg/status
```

**Na ISO final (boot de teste):**

```bash
ps -p 1 -o comm=            # deve imprimir: systemd
systemctl status getty@tty1.service        # autologin do kiosk no TTY1
systemctl show systemd --property=RuntimeWatchdogUSec   # watchdog ativo
dpkg -l live-config-systemd live-config-sysvinit 2>/dev/null
# live-config-systemd = ii ; live-config-sysvinit = não instalado (ou ausente)
```

---

## 7. Se ainda quebrar (troubleshooting)

1. **`Package 'live-config-sysvinit' has no installation candidate` no meio do
   build** → algum manifesto seu lista o pacote (ou um pacote de terceiros depende
   dele sem alternativa). O auditor (item 4) aponta o arquivo; remova a entrada ou
   ajuste o `.pref` (uma dependência dura sem alternativa não pode ser bloqueada —
   precisa ser removida da árvore de pacotes).
2. **Hook da Camada 3 não aparece no build.log** → o live-build só executa
   `config/hooks/live/*.chroot` se também existir ao menos um hook em
   `config/hooks/normal/*.chroot` (condição do `chroot_hooks`). O kit já traz o
   guarda `0091-ensure-live-hooks-run.hook.chroot` em `hooks/normal/` exatamente
   para isso — se o problema persistir, confirme que o guarda foi copiado junto
   (o auditor verifica ambos no bloco [C]) e que o hook tem permissão de execução
   (`chmod +x`).
3. **Persistir um erro do resolvedor com outro pacote** → colete a transação
   exata com `chroot chroot apt-get -o Debug::pkgProblemResolver=yes install …`
   e `apt-cache rdepends`; o solver 3.0 do trixie pode expor conflitos latentes
   de terceiros que o antigo resolvedor mascarava.
4. **Usa `live-task-*` ou `tasksel`?** Os metapacotes `live-task-*` do Debian Live
   são compatíveis com o kit (a pinagem decide o backend por você), mas mantenha-os
   sob a auditoria — qualquer referência a sysvinit aparece no relatório.
5. **Quer o bloqueio dentro do kiosk em produção?** Renomeie
   `00-block-sysvinit.pref.chroot` → `00-block-sysvinit.pref` (sem sufixo) para o
   pin persistir na ISO final.

---

## Apêndice — Fatos verificados no live-build `1:20250505+deb13u1` (trixie)

Estados e comportamentos abaixo foram lidos diretamente dos scripts do pacote — não
são suposições:

- **Ordem do estágio chroot:** `chroot_cache restore` → `chroot_prep install all
  mode-archives-chroot` (aplica sources, `apt.conf`, **preferences** e monta
  devpts/proc/sysfs) → `chroot_linux-image` → `chroot_firmware` → `chroot_preseed` →
  `chroot_includes_before_packages` → **passes** `chroot_package-lists` +
  `chroot_install-packages` (primeiro `install`, depois `live`) →
  `chroot_includes_after_packages` → **`chroot_hooks`** → `chroot_hacks` →
  `chroot_interactive` → `chroot_prep remove` (desmonta e **remove o `.pref.chroot`
  da imagem final**).
- **Passes de lista:** `*.list.chroot_install` só no passe 1; `*.list.chroot_live`
  só no passe 2; `*.list.chroot` entra nos dois. Instalação = um único
  `apt-get install` por passe, com todos os pacotes via `xargs --arg-file`.
- **Remoção em lista:** só com hífen **final** (`pacote-`); hífen inicial é erro de
  option parsing do `apt-get`.
- **Pinagem:** `config/archives/*.pref.chroot` → `chroot/etc/apt/preferences.d/`
  durante o build (removido da ISO final); `*.pref` (sem sufixo) → também na ISO
  final; caminho alternativo válido: `config/apt/*.pref` (aplicado pelo
  `chroot_apt`, mesmo momento do `chroot_prep`).
- **Hooks:** `config/hooks/{normal,live}/*.chroot` (o sufixo `.hook.chroot` casa
  com o glob `*.chroot`), executados em ordem lexicográfica (normal antes de live,
  dentro de cada diretório), DENTRO do chroot, após a instalação dos pacotes;
  `.container` roda via `systemd-nspawn`. Execução exige hooks também em
  `config/hooks/normal/` — por isso o kit inclui o guarda 0091.
- **Bootstrap:** o estágio bootstrap é `debootstrap` puro (não há aplicação de
  preferences nem instalação de `live-config` — Priority: optional). O
  `systemd-sysv` (Priority: important) vem do debootstrap — base correta do kiosk.
