# Verificações desta revisão

Executados no ambiente de desenvolvimento:

- `cargo check --locked`: compilação Rust e Slint.
- `cargo test --locked`: 24 testes aprovados; inclui fila cheia sem débito,
  reabertura de banco com compra pendente, estorno idempotente sem inflar caixa,
  cópia USB atômica e preservação de seleção em catálogo de 5.000 álbuns.
  Inclui pacotes/bônus, saldo fracionado, recebimentos duplicados, antirrepetição,
  Festa sem débito/estorno indevido, senha e regras do Attract. Um teste GStreamer
  verifica frames RGB limitados e efeito secundário com música em Playing.
- GUI em Xvfb, renderer software, áudio `fakesink` sincronizado: aviso de falta
  de crédito, badge CLIPE, fundo de vídeo em áudio, tela cheia após 10 segundos,
  volume sobre vídeo ainda avançando, autenticação e configurações permanecendo
  abertas após mais de 10 segundos. Contraste de Festa corrigido após inspeção.
- `python3 -m unittest discover -s tests`: 8 testes aprovados do cliente de
  acervo (HTTP simulado), incluindo retomada, servidor sem Range, hash inválido,
  troca atômica do diretório e proteção do acervo local.
- `build_iso.sh --check`, auditoria dos manifestos systemd e sintaxe dos scripts.

O Rust usado foi 1.90.0. As bibliotecas nativas foram extraídas de pacotes
Ubuntu 24.04 para um sysroot temporário, pois a instalação global não estava
disponível. Isso verifica o aplicativo, mas não substitui compilar a imagem no
Debian 13 conforme o Dockerfile.

Ainda não executados: geração/boot da ISO, instalador em disco real, testes de
Wi-Fi, áudio/vídeo sob carga, medição de RAM com imagens reais e homologação com
servidor de acervo de produção. O protocolo documentado precisa ser atendido pelo
servidor ou por um adaptador. A integração de saldo Pix com o serviço do operador continua pendente; o feedback de crédito foi ligado à confirmação local.

## Ajustes da revisão do PR #4

- Consulta de receita com erro mostra Indisponível e registra o erro; zero real
  continua sendo R$ 0,00. Teste de regressão adicionado.
- Teste de mídia usa o mesmo CreditArpeggio do comando CreditFx: verifica
  deadline, substituição do agendamento por entradas rápidas, cancelamento da
  nota aguda anterior, EOS e música permanecendo em Playing. O tempo do
  agendador é controlado no teste, sem depender de sleeps de 75 ms.
- 8 testes Python passaram e git diff --check passou nesta revisão.
- Testes Rust/GStreamer desta atualização não foram executados: toolchain e
  bibliotecas nativas indisponíveis; instalação de dependências bloqueada pelo
  ambiente. Executar cargo test --locked antes do merge e homologar ALSA real.
