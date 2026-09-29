# Teste com um álbum no Cloudflare R2

Este piloto usa o contrato de `ONLINE-CATALOG.md`. O script local só prepara
arquivos; ele não cria a assinatura R2, o bucket nem solicita credenciais.
Não use o publicador de um álbum para gerenciar o acervo completo: cada execução
gera um índice com **somente aquele álbum**. O cliente preserva discos locais,
mas esse fluxo ainda não é um gerenciador de catálogo de produção.

1. No painel R2, use o bucket **Standard** `jukebox`. Para testar um único álbum,
   use a URL pública `https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev`.
   Este endereço é diferente do **S3 API**: o S3 API serve para enviar
   arquivos com credenciais; a URL pública serve para a jukebox baixar os arquivos.
   No painel, gere um token de API S3 limitado a esse bucket, com permissão de
   escrita. Guarde as credenciais fora do repositório e **nunca na jukebox**.
   O endereço `r2.dev` é apenas para este piloto; para produção, conecte um
   domínio próprio ao bucket e desative o acesso `r2.dev`.
2. No PC, escolha um álbum MP3 na pasta `Artista/Nome do Álbum/`, sem subpastas.
   Execute o gerador incluído no repositório:

   ```sh
   python3 distro/tools/build-r2-pilot.py \
     '/caminho/Artista/Nome do Álbum' \
     --artist 'Artista' --genre 'Sertanejo' \
     --base-url 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev' \
     --out '/tmp/jukebox-r2-pilot'
   ```

3. Instale o AWS CLI no PC e execute `aws configure --profile jukebox-r2`.
   Digite o Access Key ID e o Secret Access Key **apenas no prompt local**;
   para região, use `auto`. O comando abaixo envia
   **primeiro** faixas e manifesto e **depois** o índice:

   ```sh
   aws s3 cp /tmp/jukebox-r2-pilot/albums/ s3://jukebox/albums/ \
     --recursive --profile jukebox-r2 \
     --endpoint-url https://3980df26cd4bda7486ef4764f3bd5be0.r2.cloudflarestorage.com
   aws s3 cp /tmp/jukebox-r2-pilot/index.json s3://jukebox/index.json \
     --content-type application/json --cache-control 'no-cache' \
     --profile jukebox-r2 \
     --endpoint-url https://3980df26cd4bda7486ef4764f3bd5be0.r2.cloudflarestorage.com
   ```

   Para confirmar a publicação: `curl -f https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev/index.json`.
   Abra também a `manifest_url` do índice e uma `url` do manifesto. Se houver
   cache no domínio, um índice antigo pode persistir; ajuste a regra de cache
   do `index.json` para revalidar. Não substitua arquivos de versões antigas.
   Se o `curl` retornar 200 mas o script Python receber HTTP 403, confirme que
   `catalog_sync.py` envia `User-Agent: jukebox-catalog/1.0`; o endereço público
   de teste recusou a identificação padrão `Python-urllib` nesta instalação.

4. No **computador de teste** (não na máquina de produção), acrescente as linhas
   abaixo a `~/.config/jukebox/jukebox.env` (arquivo com permissão `0600`).
   O script `distro/tools/run-test-pixlogic.sh` carrega esse arquivo ao iniciar:

   ```sh
   JUKEBOX_CATALOG_URL=https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev/index.json
   JUKEBOX_CATALOG_INTERVAL=60
   JUKEBOX_DOWNLOAD_KIB=0
   ```

   Reinicie o aplicativo com `sh distro/tools/run-test-pixlogic.sh` e confira o álbum no
   carrossel e os arquivos em `jukebox-app/dados/musicas/Sertanejo/Artista/Nome do Álbum`.
   O valor `0` remove o limitador do cliente a partir da versão atualizada de
   `catalog_sync.py`; no script antigo, zero causa divisão por zero ao baixar.
   Na distro, as mesmas linhas ficam em `/dados/jukebox.env` e o destino é
   `/dados/musicas`. Faça o teste com nomes de pastas que ainda não existam no
   acervo local: o cliente protege pastas de origem USB contra sobrescrita.

5. Depois, altere uma faixa na origem, execute novamente o gerador e repita o
   envio. O ID permanece; a versão muda. A máquina baixa apenas a nova versão.
   A remoção de álbuns do servidor ainda não remove arquivos locais.

**Cuidado com custos e exposição:** um bucket público permite que qualquer pessoa
com o endereço acesse os objetos. O protocolo atual não autentica cada máquina.
Limite o piloto a poucos arquivos e acompanhe o consumo no painel R2. A taxa
informada para Standard é US$ 0,015/GB-mês acima dos primeiros 10 GB; 500 GB
armazenados durante um mês seriam aproximadamente US$ 7,35 de armazenamento,
antes de tributos e eventuais operações excedentes.
