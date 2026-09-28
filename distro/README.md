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
`/dados/pix/qr.png` (PNG de 128 a 2048 px por lado, até 4 MiB, com a borda
branca do código preservada). Prefira baixar a imagem oficial do Mercado Pago;
se houver apenas PDF, extraia somente a área do QR em PNG sem alterar seu
conteúdo. Copie primeiro para um arquivo temporário em `/dados/pix/`, renomeie
para `qr.png` quando estiver completo e reinicie a jukebox. Sem a imagem, a
tela indica que o cliente deve usar o QR físico. A imagem continua visível
quando não há conexão com o PixLogic; o estado de conexão aparece separado.
Antes de colocar em operação, escaneie os códigos da tela e do impresso e
confira no aplicativo de pagamento se ambos correspondem à máquina correta.

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
