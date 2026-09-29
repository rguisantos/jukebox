# Publicação de vários álbuns no R2

O publicador de lotes trabalha com o bucket `jukebox` e a URL pública do teste.
Use **um diretório de entrada só com os álbuns novos** para evitar copiar e
verificar toda a coleção a cada publicação. A estrutura é:

```text
entrada/
  Vitinho Imperador/
    Novo Álbum/
      01 - Primeira.mp3
      02 - Segunda.mp3
      capa.jpg
  Outro Artista/
    Outro Álbum/
      01 - Faixa.mp3
```

O programa lê o gênero `TCON` das tags ID3v2.3/v2.4 dos MP3. Para um álbum
com tags diferentes ou sem gênero, crie `~/jukebox-generos.json`:

```json
{
  "artists": {
    "Vitinho Imperador": "Piseiro"
  },
  "albums": {
    "Outro Artista/Outro Álbum": "Sertanejo"
  }
}
```

A prioridade é: gênero definido para o álbum, gênero definido para o artista,
tags das músicas. `--default-genre` é opcional e só vale quando nenhuma faixa
tem gênero. Havendo tags divergentes, defina o álbum explicitamente no JSON.
Use exatamente os nomes das pastas no JSON. A jukebox usa a pasta de gênero
como classificação do álbum, independentemente da tag de cada faixa.
Os nomes devem estar cadastrados no publicador de escritório. O CLI usa o mesmo
arquivo `~/.config/jukebox-publisher/genres.json`, ou os gêneros padrão quando
ele não existe. `--genre-policy` permite indicar outro cadastro. Sinônimos são
normalizados; combinações de estilos distintos exigem uma escolha explícita.

Outra opção é organizar a entrada em `Gênero/Artista/Álbum` e acrescentar
`--layout genre-artist-album` ao comando. Nesse formato, **a pasta Gênero manda**,
sem depender das tags ou do JSON; artistas de gêneros diferentes podem estar
em pastas de gênero distintas. Use um formato por lote.

```sh
python3 ~/Downloads/publish-r2-catalog.py \
  '/home/bilhares/Músicas/entrada-por-genero' \
  --layout genre-artist-album \
  --base-url 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev' \
  --out '/tmp/jukebox-lote-por-genero-01'
```

Baixe o arquivo `publish-r2-catalog.py` para o computador. Não é necessário
instalar módulos Python adicionais. Aponte para a entrada e prepare o lote:

```sh
python3 ~/Downloads/publish-r2-catalog.py \
  '/home/bilhares/Músicas/entrada-jukebox' \
  --genres "$HOME/jukebox-generos.json" \
  --base-url 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev' \
  --out '/tmp/jukebox-lote-01'
```

O arquivo de gêneros é opcional; se todas as tags tiverem gêneros consistentes,
remova a opção `--genres`. O diretório de saída precisa ser **novo e vazio**.
O programa consulta o índice atual do R2, conserva os discos existentes,
calcula hashes dos novos álbuns e mostra o resultado. Revise a listagem e o
`/tmp/jukebox-lote-01/index.json` antes de publicar.

Publique o lote preparado com o token já configurado no perfil `jukebox-r2`:

```sh
python3 ~/Downloads/publish-r2-catalog.py \
  --upload-prepared \
  --base-url 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev' \
  --out '/tmp/jukebox-lote-01'
```

O publicador confere se o índice remoto não mudou desde o preparo. Ele envia
faixas/capas e manifestos primeiro; publica `index.json` por último. Se a
rede falhar antes do índice, o catálogo anterior continua ativo; repita o
comando para retomar. Depois confira:

```sh
curl -f 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev/index.json'
```

Em cada lote seguinte, use **outro** nome de saída, por exemplo
`/tmp/jukebox-lote-02`. O publicador pode atualizar um álbum existente no
mesmo gênero, mantendo seu ID e criando uma versão nova. Para editar gênero,
artista ou título de um álbum já publicado, use **Acervo do servidor…** no
publicador de escritório (`PUBLICADOR-ESCRITORIO.md`). Atualize primeiro o
sincronizador e o aplicativo das jukeboxes para migração das pastas e limpeza
dos registros antigos.
Uma retirada do índice também não apaga músicas já baixadas nas máquinas.
Capas devem ser pequenas; cada lote ocupa espaço temporário equivalente aos
arquivos copiados. Para centenas de álbuns, publique em lotes menores.

O URL `r2.dev` é só para este piloto. Para produção, use domínio próprio e
planeje autenticação de máquinas antes de publicar um acervo grande.
