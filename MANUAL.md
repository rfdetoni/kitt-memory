# Manual do K.I.T.T. Memory (`kitt-memory`)

> Autoridade única de memória persistente do ecossistema K.I.T.T., em Rust + SQLite WAL/FTS5.

## Componentes

- `kitt-memory-core`: domínio, ranking, sensibilidade, correções, conceitos, links e contratos semânticos.
- `kitt-memory-sqlite`: persistência local concorrente, FTS5 e transações.
- `kitt-memoryd`: serviço loopback autenticado consumido pelo Agent e pelo Assistant quando aplicável.

A linha 0.5.x não mantém importadores de bases antigas do Agent. O contrato suportado é apenas o do ecossistema K.I.T.T. atual.

## Build e validação

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release -p kitt-memoryd
cargo run --release -p kitt-memory-sqlite --example retrieval_benchmark -- 2000 250
```

Requisito: Rust 1.88+.

## Serviço

Por padrão `kitt-memoryd` escuta em `127.0.0.1:41829`.

Variáveis suportadas:

- `KITT_MEMORY_ADDR`
- `KITT_MEMORY_CONFIG_DIR`
- `KITT_MEMORY_DATA_DIR`
- `KITT_MEMORY_TOKEN_PATH`
- `KITT_MEMORY_DB`

O banco e o token são privados ao usuário; symlinks para o banco são rejeitados.

## Contrato de gerenciamento

O plano de leitura recomendado é `memory.search` → `memory.get`, com `memory.timeline` para continuidade temporal. `memory.search` aceita opcionalmente `include_provenance=false` e `exclude_ids`; a busca por si só não incrementa telemetria de acesso.

`memory.baseline.request` devolve baseline determinístico com `baseline_revision` e `etag`. O cliente pode reenviar `if_none_match`; quando nada semanticamente relevante mudou, a resposta retorna `not_modified=true` sem retransmitir as entradas.

Além de remember/recall/forget, `memory.manage` expõe:

- `list`, `get`, `set_status`, `pin`, `touch`, `archive_workspace`;
- `dream.last`, `dream.record`, `dream.commit`, `maintenance`;
- `correction.record`;
- `concept.upsert`;
- `concept.link`.

Correções e conhecimento reutilizável são persistidos somente aqui; o Agent não mantém tabelas paralelas.

## Segurança e privacidade

Sensibilidade é monotônica, escopos são isolados por namespace/workspace/conversa e writes críticos usam transações SQLite `IMMEDIATE`. O consumidor continua responsável pela política de egress antes de enviar memória a modelos remotos.


## SQLite schema v9 e manutenção

A migração v9 recalcula normalização/hashes e deriva `gist` + `tokens_est`; colisões novas são consolidadas por supersessão, preservando a sensibilidade mais restritiva. O FTS quente indexa apenas o conteúdo normalizado.

O daemon reutiliza um pool pequeno de readers, mantém um worker pool fixo para conexões e bufferiza `recall_traces`. A cada minuto executa manutenção limitada: flush de traces, prune de expirados e histórico antigo, `FTS optimize`, `PRAGMA optimize` e checkpoint WAL passivo.
