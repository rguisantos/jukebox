# Verificações desta revisão

Executados no ambiente de desenvolvimento:

- `cargo check --locked`: compilação Rust e Slint.
- `cargo test --locked`: 14 testes aprovados; inclui fila cheia sem débito,
  reabertura de banco com compra pendente, estorno idempotente sem inflar caixa,
  cópia USB atômica e preservação de seleção em catálogo de 5.000 álbuns.
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
servidor ou por um adaptador. Não há alteração de lógica do Pix.
