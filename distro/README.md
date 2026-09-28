# Jukebox OS — Debian 13 mínimo

A configuração do appliance está versionada em `config/`. A imagem contém
Xorg/Openbox, Rust/Slint, GStreamer/ALSA, NetworkManager, firmwares Wi-Fi e
ferramentas de instalação. Não há ambiente desktop completo.

## Compilação

Use Debian 13 para compilar o aplicativo e a imagem com as mesmas bibliotecas.
`build_iso.sh --check` verifica os arquivos mínimos sem instalar pacotes.

Ambiente de build isolado:

```sh
docker build -t jukebox-builder -f distro/Dockerfile .
docker run --rm --privileged -v "$PWD:/src" jukebox-builder
```

O container precisa de privilégios para as montagens/chroot do live-build.
Não execute em host compartilhado não confiável. O script não modifica o
live-build instalado no host. A saída padrão é `distro/live-image-amd64.hybrid.iso`.
O Rust fica fixado em 1.90.0 e a compilação usa `Cargo.lock` com `--locked`.
Os pacotes Debian acompanham as atualizações do repositório: o build ainda não
é idêntico byte a byte entre datas diferentes.

A ISO deve ser validada em BIOS e UEFI sem Secure Boot e nas placas reais antes
de distribuição. A verificação estática não comprova boot, áudio, GPU ou Wi-Fi.

## Clipes aleatórios para músicas sem vídeo

A distro cria `/dados/fundos/` na partição `JUKEBOX_DATA`. Coloque ali clipes
`.mp4`, `.mpeg` ou `.wmv` (inclusive em subpastas). O player escolhe um vídeo
aleatório, sem som, enquanto toca uma música só de áudio. Ao terminar um clipe,
escolhe outro; músicas que já têm vídeo mostram o próprio vídeo. Sem clipes,
a música continua normalmente, sem imagem de fundo.

Para instalar pelo pendrive, crie uma pasta **`fundos` na raiz do pendrive**,
com os vídeos dentro. Exemplo: `fundos/festa.mp4`. Ao inserir o pendrive ou
forçar a sincronização no menu do operador, esses arquivos vão para
`/dados/fundos/`, fora de `/dados/musicas/` e do carrossel. O player
recarrega a lista após a importação, inclusive quando uma música já estiver
tocando. Vídeos nas demais pastas do pendrive entram no catálogo como faixas.
Se uma versão anterior importou clipes para `/dados/musicas/fundos/`, eles
também poderão servir de fundo, mas serão ocultados do carrossel e do modo
aleatório de músicas. Após copiar os clipes para `/dados/fundos/`, o operador
pode retirar a cópia antiga para recuperar espaço em disco.

## Inicialização e armazenamento

`systemd → jukebox-data → sessão Xorg/Openbox → aplicativo`.
O systemd reinicia a sessão após saída/crash em 3 segundos. Isso não detecta
travamento interno de um processo ainda vivo; watchdog de saúde/hardware é
uma evolução posterior.

Uma partição ext4 com label `JUKEBOX_DATA` é montada em `/dados`. Ela mantém
músicas, capas, fila/créditos, configuração e conexões do NetworkManager.
Sem essa partição, a sessão live serve para demonstração e seus dados podem
ser descartados no reboot. Não use esse modo para receber dinheiro.

A configuração da máquina é `/dados/jukebox.env`. Não coloque comandos nesse
arquivo: ele é carregado pelo launcher como configuração shell.
O launcher verifica se `/dados` está montado. Se não estiver, desativa Pix,
PixLogic e pulsos de dinheiro e informa na tela que pagamentos estão
indisponíveis. A reprodução de demonstração continua disponível.
Para usar a entrega automática de créditos pelo PixLogic, configure os três
valores `JUKEBOX_PIXLOGIC_API` (origem HTTPS, sem `/api`),
`JUKEBOX_PIXLOGIC_UUID` e `JUKEBOX_PIXLOGIC_TOKEN` (credencial de 64 caracteres).
A jukebox consulta `/api/machine/{uuid}/credit` a cada 3 segundos, grava a
operação no SQLite e confirma pelo endpoint `/confirm`. Confirmações não
concluídas são retomadas depois de reiniciar. Mantenha o token restrito ao
operador. O protocolo não fornece QR: use o QR associado à máquina no PixLogic.
Sem os três valores, a jukebox mantém o serviço de QR dinâmico anterior.

Para mostrar na tela o mesmo QR estático impresso, instale a imagem pública em
`/dados/pix/qr.png` (PNG de 128 a 4096 px por lado, até 8 MiB, com a borda
branca do código preservada). Prefira baixar a imagem oficial do Mercado Pago;
se houver apenas PDF, extraia somente a área do QR em PNG sem alterar seu
conteúdo. Copie primeiro para um arquivo temporário em `/dados/pix/` e renomeie
para `qr.png` quando estiver completo. A jukebox verifica alterações a cada
5 segundos, sem precisar reiniciar. Se faltar a imagem ou ela for inválida,
a tela informa o motivo e orienta usar o QR físico. A imagem continua visível
quando não há conexão com o PixLogic; o estado de conexão aparece separado.
Confira se o PixLogic está configurado e se a partição /dados está montada; a imagem não ativa pagamentos quando o serviço está desativado.
Antes de colocar em operação, escaneie os códigos da tela e do impresso e
confira no aplicativo de pagamento se ambos correspondem à máquina correta.

### Máquina PixLogic de teste

O perfil público `test-machines/pixlogic-b4450dba.env` contém a origem e o UUID
da máquina de teste. O repositório é público: não adicione nele o token do
firmware. Com o `.ino` dessa máquina disponível **localmente**, instale o perfil
e a credencial direto em `/dados/jukebox.env`:

```sh
sudo python3 distro/tools/install-test-pixlogic.py /caminho/para/maquina.ino
```

Execute a partir da raiz do repositório, com `JUKEBOX_DATA` montada. O script
confere a identidade da máquina, preserva as outras opções de `jukebox.env` e
grava o arquivo com permissão `0600`, sem mostrar o token. Desligue a ESP que
usa esse mesmo UUID antes de iniciar a jukebox: dois clientes simultâneos
podem disputar o mesmo crédito reservado. Reinicie a jukebox após instalar.

No PC de desenvolvimento, sem partição `JUKEBOX_DATA`, execute **sem sudo**:

```sh
python3 distro/tools/install-test-pixlogic.py \
  /home/bilhares/Downloads/sistemapix-b4450dba-3931-486c-b7ef-57709e724684.ino --local
sh distro/tools/run-test-pixlogic.sh
```

O perfil fica em `~/.config/jukebox/jukebox.env`, fora do repositório. O script
de execução carrega as variáveis no processo e inicia `cargo run --release` na
pasta do aplicativo. Se `/dados` existir sem estar montado, verifique esse
diretório antes de receber créditos: o app poderia escolhê-lo como local do
banco de dados. Em qualquer modo, não rode a ESP e a jukebox com o mesmo UUID
ao mesmo tempo.

## Instalar no disco

Inicie pela ISO. Com teclado de manutenção, execute:

```sh
sudo /usr/local/sbin/jukebox-install-to-disk /dev/DISCO
```

O instalador mostra o disco e exige a confirmação literal antes de apagar.
Recusa discos montados ou menores que 20 GiB. Cria GPT com BIOS boot, EFI,
12 GiB de sistema e restante para dados, extrai a imagem e instala GRUB BIOS/UEFI.
Não foi executado em disco real neste desenvolvimento.

No sistema instalado, overlayroot protege a raiz com overlay temporário;
`/dados` permanece gravável. Atualizações online alteram apenas o acervo,
não o sistema operacional. Para manutenção permanente, desabilite overlayroot
pelo parâmetro de boot `overlayroot=disabled`, faça a manutenção e reative.

## Wi-Fi

Menu do operador → CONFIGURAR WI-FI abre `nmtui-connect` em uma janela de
manutenção. Use teclado para selecionar a rede e informar a senha. Não é
um teclado virtual nem uma tela nativa Slint. Ao sair, retorna à jukebox.
NetworkManager salva a conexão e reconecta automaticamente; os perfis ficam
em `/dados/network` com acesso exclusivo de root. Ethernet continua disponível.
Adaptadores devem ser homologados com os firmwares presentes na imagem.

## Acervo online

Veja `ONLINE-CATALOG.md`. Configure o endereço HTTPS do índice no arquivo da
máquina. O cliente consulta ao iniciar e periodicamente. O menu do operador
permite verificar manualmente e mostra progresso/erro. O aplicativo continua
reproduzindo enquanto o worker baixa os arquivos, com taxa limitada.

## USB

O udev solicita uma unidade systemd por partição USB, montada somente para
leitura em `/media/usb/DISPOSITIVO`. Arquivos preservam a organização
`Gênero/Artista/Álbum/arquivo`. Capas JPG/PNG/WebP também são copiadas.
Arquivos são publicados com rename após cópia e fsync. Arquivos existentes
com o mesmo tamanho são ignorados; não há reconciliação por hash para USB.
Não altere pelo USB os álbuns gerenciados pelo servidor.

## Comportamento após falha

A reserva da música e o débito são uma transação SQLite. Limite: 20 compras
pendentes, incluindo a faixa atual. Após reinício, a faixa interrompida recomeça
do início e as próximas continuam na ordem; não há retomada por segundo.
Falhas de arquivo/pipeline devolvem o crédito sem aumentar os contadores de
arrecadação. Pular manualmente a faixa consome a compra. A fila é removida
ao término; numa queda imediatamente antes da confirmação, a faixa pode repetir.

O banco usa WAL + synchronous=FULL. Durabilidade ainda depende do armazenamento
cumprir fsync. Os atalhos do operador foram preservados.

## Limites atuais

As imagens da interface ficam limitadas a 49 álbuns próximos da seleção
(~9,2 MiB de pixels RGB, sem contar texturas e overhead). O catálogo de metadados
ainda é carregado inteiro; medir com o acervo real de 5.000 álbuns é obrigatório.
O renderer por software está compilado: configure `SLINT_BACKEND=winit-software`
para testar GPUs incompatíveis. Não há medição de RAM total/boot nesta versão.
