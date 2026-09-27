# Núcleo, mídia e operação

## Configuração protegida

Pressione **X**, cadastre uma senha de 4–16 letras/números na primeira abertura
e confirme com Enter. Nas próximas aberturas, informe a senha. Escape cancela.
Cinco erros bloqueiam novas tentativas por 30 segundos (contador em memória).
A senha é armazenada com salt aleatório e PBKDF2-SHA256, nunca em texto puro.
Prepare a senha antes de colocar a máquina em atendimento.

No menu, W/Q navegam, O confirma e U sai. A última opção abre as configurações
avançadas. Use teclado de manutenção ou mouse nos campos, salve e clique em
“Voltar ao menu”; U então encerra a sessão. Menus administrativos não fecham
por inatividade. A reprodução que já estava em andamento continua.

## Dinheiro e créditos

Há três pacotes, configurados em **centavos inteiros** e créditos. Exemplo:
100 → 1, 500 → 6, 1000 → 14. Os preços devem ser múltiplos crescentes do
anterior e manter ou melhorar os créditos por real. A conversão usa primeiro
o maior pacote; R$ 16,50 neste exemplo concede 21 créditos e preserva R$ 0,50.
O custo de cada música em créditos continua ajustável no menu existente.

A tecla/pulso Z adiciona o valor configurado por pulso (padrão R$ 1,00).
Depósitos acumulados **antes de uma seleção bem-sucedida** recebem o mesmo
bônus de um depósito único. A seleção encerra essa acumulação, preservando
centavos insuficientes para o menor pacote. Alterar os pacotes também encerra
a acumulação com a tabela anterior. Créditos já concedidos permanecem.

O banco registra cada recebimento com identificador único, valor em centavos
e créditos concedidos. Repetir o identificador não duplica crédito. O Pix
existente ainda entrega créditos prontos: sua futura integração com saldo em
reais deverá chamar a conversão com o identificador estável da transação.
Os contadores legados do caixa continuam em créditos; `cash_receipts` registra
os valores monetários dos pulsos. O efeito sonoro/visual ocorre após confirmar
a gravação do crédito no banco.

Não é possível adicionar uma faixa igual à última da fila, incluindo a faixa
atual quando não há outras pendentes. A sequência A/B/A é permitida. Recusas
por repetição, falta de crédito ou fila cheia não debitam saldo.

O menu do operador exibe, junto ao odômetro de créditos, a receita total
acumulada em reais, identificada como **Moedeiro** — a soma idempotente dos recebimentos de `cash_receipts`
formatada como `R$ X.XXX,XX`. Esse odômetro patrimonial nunca zera, nem com
o recolhimento do caixa parcial, e serve de conferência com o moedeiro e com
o futuro relatório de PIX. Créditos concedidos por bônus de pacote não o
afetam: apenas os recebimentos registrados entram na soma. Não inclui o Pix
atual nem recebimentos anteriores à criação desse registro. A atualização ocorre
ao abrir o menu. Falhas de consulta mostram “Indisponível”, nunca R$ 0,00.

## Navegação e vídeo

- O alterna gêneros na tela de álbuns e seleciona música na lista de faixas.
- Q/W/E/R/I/O reabrem o layout e reiniciam os 10 segundos de navegação.
- Com vídeo disponível, 10 segundos sem navegação mostram o vídeo em tela cheia.
- P abre o volume sobre o vídeo; ajustes de volume não pausam a reprodução.
- “Tocada Anterior” aparece abaixo de “Tocando Agora”; inclui faixas puladas.
- O badge CLIPE identifica extensões MP4/MPEG/WMV do catálogo.

Copie loops MP4/MPEG/WMV para **`/dados/fundos`** e salve as configurações ou
reinicie para reler a pasta. Durante faixas de áudio, fundos aleatórios sem som
são reproduzidos continuamente; falhas são descartadas até a próxima releitura.
Sem fundos, o catálogo permanece visível. Esta revisão não distribui loops nem
inclui essa pasta no protocolo de atualização online de álbuns/capas.

O vídeo agora é composto dentro do Slint, permitindo overlays. O pipeline limita
a imagem a 640×360, 20 fps e um frame pendente para controlar memória e carga.
É uma escolha de desempenho que precisa ser medida na máquina mais lenta.
O ALSA `dmix` da distro mistura música e aviso de crédito na placa 0; ajuste
`/etc/asound.conf` se a saída de áudio do equipamento usar outra placa.
O aviso de crédito é um arpejo ascendente de duas notas (C5 → C6, ~150ms),
gerado internamente e reproduzido pela mesma rota do dmix. O intervalo nominal
entre notas é 75 ms; operações de banco/mídia podem atrasar a segunda nota.
Entradas rápidas reiniciam as duas notas e substituem o agendamento anterior,
sem acumular uma fila de efeitos ou modificar os créditos recebidos.

## Automação e armazenamento

Attract fica desativado por padrão (0). Configure X minutos para tocar uma faixa
aleatória quando a fila estiver vazia e não houver interação pelo intervalo.
Após a faixa terminar, aguarda outro intervalo. Festa toca continuamente sem
consumir créditos, inclusive nas escolhas do público. Compras anteriores à
ativação mantêm seu débito original; faixas gratuitas não geram estorno.
A fila do usuário tem prioridade; uma compra confirmada interrompe Attract.
Nenhuma nova faixa automática começa enquanto o operador está no menu.

A escolha automática evita a faixa anterior se houver alternativas; com uma
única faixa, ela pode repetir. Erros têm espera mínima de cinco segundos.

O espaço disponível é consultado a cada 30 segundos. Abaixo do limite configurado
(padrão 1024 MiB), o cabeçalho e o vídeo em tela cheia exibem alerta. A checagem
não apaga arquivos. Sincronização e importação preservam suas verificações próprias.

## Recuperação e reindexação

“Zerar créditos” também descarta centavos e progresso de bônus ainda acumulados,
na mesma transação. Recibos, caixa e odômetro são preservados.

Se a finalização de uma faixa falhar no banco, o player suspende a reprodução e
tenta concluir novamente a cada dois segundos, além do tempo gasto na consulta.
A primeira decisão de estorno é preservada mesmo que U seja pressionado durante
a recuperação. A próxima faixa só inicia após a confirmação no banco.

O scanner compara tamanho, inode, dispositivo e horários de modificação/alteração
com precisão de nanossegundos. Arquivos novos ou modificados têm suas tags relidas;
faixas existentes mantêm o identificador. Na primeira varredura após a atualização,
o catálogo anterior ganha essas assinaturas, exigindo uma releitura das tags.
Arquivos alterados durante a leitura ficam para a próxima varredura. Isso não
remove faixas ausentes nem altera a fila já reservada.

O workflow Validate jukebox compila Rust/Slint e executa testes de GStreamer com
saída simulada, além dos testes de sincronização, em cada PR e atualização da main.
Ele não substitui os testes de queda de energia e ALSA no equipamento real.

## Importação e sincronização do acervo

Importações USB e atualizações online compartilham a trava de publicação e
varredura `.catalog-sync.lock`. Uma atualização online aguarda a importação USB
terminar; a interface continua responsiva. USB compara o SHA-256 dos arquivos
de mesmo tamanho antes de ignorar uma cópia, então correções de faixas/capas
com o mesmo número de bytes são importadas. Isso lê os arquivos completos no
pendrive e no disco durante uma importação, o que pode prolongar a operação.
O atualizador também informa falhas de seu processo, mesmo quando já publicou
um álbum válido antes de falhar.
