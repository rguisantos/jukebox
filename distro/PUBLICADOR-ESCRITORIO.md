# Publicador de álbuns no computador do escritório

Este programa abre uma janela para escolher pastas, organizar artistas e discos,
ajustar gêneros e enviar um lote ao bucket R2 `jukebox`. Ele não armazena chaves
de acesso; usa o perfil `jukebox-r2` já criado no AWS CLI.

## Instalação

No Linux do escritório, mantenha **os dois scripts na mesma pasta**:

- `distro/tools/jukebox-publisher-gui.py`
- `distro/tools/publish-r2-catalog.py`

É necessário Python 3 com Tkinter (`sudo apt install python3-tk` no
Ubuntu/Debian, se o sistema não o tiver) e
AWS CLI configurado com o perfil `jukebox-r2`. Abra a aplicação com:

```sh
cd /home/bilhares/jukebox
python3 distro/tools/jukebox-publisher-gui.py
```

Se recebeu o pacote ZIP separado do repositório, extraia-o em `~/Aplicativos`
e execute `python3 ~/Aplicativos/jukebox-publicador/jukebox-publisher-gui.py`.
Nesse caso, o instalador do atalho está na mesma pasta extraída.

Para aparecer no menu de aplicativos do Linux, execute uma vez:

```sh
python3 distro/tools/install-office-publisher.py
```

O instalador cria o atalho **Jukebox - Publicador de Álbuns** para seu usuário,
sem `sudo`. Mantenha os scripts no mesmo local após instalar o atalho.

## Fluxo do operador

1. Clique em **Adicionar pasta…** e escolha um álbum, a pasta de um artista ou
   a pasta onde o Deemix salvou os artistas. É possível adicionar mais pastas.
2. Veja a lista **Artista > Álbum**, quantidade de faixas, tamanho, gênero e
   situação. Gêneros conhecidos são lidos das tags MP3; pastas sem gênero,
   desconhecidas ou com estilos diferentes aparecem como **Definir gênero**.
   Pastas com arquivos não suportados ou CDs
   separados em subpastas aparecem como **Verificar arquivos**.
3. Selecione um álbum ou o artista inteiro. Escolha um gênero da lista e clique
   **Aplicar gênero**. Use **Gerenciar gêneros…** para cadastrar um gênero novo
   ou um sinônimo de tag. Por exemplo, `Música Religiosa` e `Gospel` usam
   apenas `Gospel` no catálogo. Tags `Música Religiosa;Gospel` são unificadas;
   `Gospel;Funk` exige que você escolha um gênero. Nomes com `;` não são aceitos.
4. Clique **Revisar e enviar ao R2**. O programa prepara as versões e mostra
   exatamente quais álbuns serão publicados. Confirme para enviar as faixas,
   capas e manifestos; o índice do catálogo é enviado por último.
5. Veja o andamento na parte inferior. Quando a publicação terminar, as
   jukeboxes consultarão o índice e baixarão as novidades automaticamente.

A seleção e os gêneros editados são guardados localmente para continuar na
próxima abertura. Selecione novamente a pasta para reler arquivos alterados.
O cadastro de gêneros fica em `~/.config/jukebox-publisher/genres.json` e
permanece no computador ao atualizar os scripts.

## Editar o que já está no servidor

Abra **Acervo do servidor…**, acima da lista de pastas locais. Selecione um
álbum, altere artista, nome e gênero e clique **Aplicar edição**. É possível
selecionar vários álbuns para aplicar um gênero em lote; artista e nome são
editados individualmente. Depois clique **Revisar e publicar edições** e
confirme o resumo. É possível editar sem ter as pastas locais das músicas.

A publicação envia somente manifestos e índice, mantendo o ID e os endereços
dos arquivos de música. Mudanças concorrentes no catálogo cancelam a operação
para que você atualize e revise. Outros álbuns são preservados.

**Antes de publicar mudanças de gênero, artista ou nome, atualize o
sincronizador de cada jukebox.** O pacote inclui `catalog_sync.py`. No ambiente
de teste que executa o repositório em `~/jukebox`, use:

```sh
cp ~/Aplicativos/jukebox-publicador/catalog_sync.py \
  ~/jukebox/distro/config/includes.chroot/opt/jukebox/catalog_sync.py
```

Se a máquina usa o sincronizador instalado em `/opt/jukebox`, atualize também:

```sh
sudo install -m 755 ~/Aplicativos/jukebox-publicador/catalog_sync.py \
  /opt/jukebox/catalog_sync.py
```

O sincronizador move a pasta do álbum para o novo gênero/artista/nome sem
baixar novamente os arquivos íntegros. Arquivos ausentes ou corrompidos são
restaurados; conflitos com pastas de outro álbum ou importadas por USB impedem
a substituição. Uma interrupção após mover a pasta é recuperada na próxima
sincronização. A interface relê o catálogo após a atualização.

Após atualizar o aplicativo Rust e reiniciá-lo, o scanner remove do catálogo
os registros dos caminhos antigos que não existem mais. Se estiver usando um
binário anterior e o álbum antigo ainda aparecer sem arquivos, feche a jukebox
e execute a ferramenta de reparo na máquina de teste:

```sh
python3 distro/tools/repair-missing-catalog.py jukebox-app/dados/jukebox.db
```

Na distro, informe `/dados/jukebox.db`. A ferramenta gera um backup antes da
limpeza e preserva créditos, Pix, configurações e fila. Reabra o programa
depois. Atualize também o binário para tornar a reconciliação automática.

O servidor já está configurado com a URL pública de teste `r2.dev` e bucket
`jukebox`. **Configurar servidor** permite trocar o perfil AWS CLI, endpoint
e URL pública ao passar para um domínio próprio. Nenhum segredo aparece nessa
tela nem é salvo pelo publicador.

O programa preserva álbuns remotos não selecionados. Não feche a janela
durante o preparo ou o envio. Cada lote
é preparado em espaço temporário; verifique o espaço livre ao trabalhar com
centenas de discos. O R2 `r2.dev` é adequado apenas para este piloto.
