# Manual do K.I.T.T. Memory (`kitt-memory`)

> Motor de armazenamento de memória persistente local com SQLite WAL, sensibilidade monotônica, isolamento de segurança e migração de schema canônico.

---

## 1. Visão Geral e Arquitetura

O **`kitt-memory`** provê o armazenamento de longa duração (long-term memory) para o assistente e o agente de codificação.
Ele gerencia memórias episódicas, decisões arquiteturais, preferências de usuário e contexto histórico de projetos.

### Componentes:
- **`kitt-memory-core`**: Definição de domínio, níveis de sensibilidade monotônica (`Public`, `Personal`, `Private`, `Secret`, `Ephemeral`) e normalização.
- **`kitt-memory-sqlite`**: Driver de alta concorrência SQLite em modo WAL com transações atômicas e permissões de arquivo privadas (`0600` em Unix).
- **`kitt-memory-migrate`**: Ferramenta CLI para inspecionar, reparar e migrar bases de dados de versões legadas para o **Schema Canônico V4**.

---

## 2. Requisitos de Sistema

- **Rust**: 1.88+ (com `cargo`)
- **SQLite**: embutido pelo `rusqlite` (`bundled`); não é necessário instalar `libsqlite3-dev` no build padrão.

---

## 3. Instalação e Compilação por Sistema Operacional

### 🐧 A. LINUX (Ubuntu/Debian/Fedora)

```bash
# Compilar workspace completo
cargo build --release --workspace

# Testar
cargo test --workspace
```

### 🍏 B. macOS

```bash
# Compilar workspace completo
cargo build --release --workspace

# Testar
cargo test --workspace
```

### 🪟 C. WINDOWS (PowerShell)

```powershell
# Compilar workspace completo via Cargo
cargo build --release --workspace

# Executar testes
cargo test --workspace
```

Os binários compilados estarão disponíveis em:
- `target/release/kitt-memory-migrate` (Linux/macOS)
- `target/release/kitt-memory-migrate.exe` (Windows)

---

## 4. Configuração e Variáveis de Ambiente

O banco de dados SQLite é criado automaticamente no primeiro acesso.

### Localização Padrão do Banco de Dados:
- **Linux**: `~/.kitt/history/history.sqlite3` ou `~/.config/kitt/memory.db`
- **macOS**: `~/Library/Application Support/kitt/memory.db`
- **Windows**: `%APPDATA%\kitt\memory.db`

O crate não interpreta uma variável própria para caminho do banco nem configura logging global; o componente consumidor define caminho e observabilidade.

---

## 5. Guia de Uso da CLI `kitt-memory-migrate`

A ferramenta `kitt-memory-migrate` realiza a migração de memórias da base legada do `kitt-agent-cli` para a base canônica do `kitt-memory`:

### Sintaxe de Execução:
```bash
cargo run --release --bin kitt-memory-migrate -- <caminho/origem-agent-cli.db> <caminho/destino-kitt-memory.db>
```

### Exemplo Prático:
```bash
# Migrar memórias históricas do agente para a base compartilhada
cargo run --release --bin kitt-memory-migrate -- \
  ~/.kitt/history/history.sqlite3 \
  ~/.config/kitt/memory.db
```
*Saída esperada:* `imported X memories`

---

## 6. Políticas de Segurança e Privacidade
1. **Permissões de Arquivo**: No Unix/macOS, o arquivo de banco é sempre criado com modo `0600` (leitura/escrita apenas pelo usuário).
2. **Rejeição de Symlinks**: Symlinks são estritamente rejeitados para evitar ataques de redirecionamento de caminho.
3. **Sensibilidade Monotônica**: Uma vez gravada como confidencial ou secreta, uma memória nunca sofre downgrade de sensibilidade por atualizações parciais.


---

## 7. Retrieval híbrido e baseline

O schema v4 mantém o banco local/FTS5 e adiciona validade temporal explícita. Cada memória pode carregar `valid_from` e `valid_until`, e consultas podem usar `as_of`; recall e baseline filtram registros fora da janela válida antes do ranking. Ao marcar uma memória como `SUPERSEDED` ou `ARCHIVED`, o store fecha a janela temporal aberta sem apagar o registro histórico.

O recall usa um conjunto de candidatos limitado, combinando sinal lexical, retenção e prioridade. Um chamador pode opcionalmente fornecer um `SemanticReranker`; se esse componente falhar ou não existir, o caminho local continua funcional.

`MemoryStore::baseline` produz um snapshot determinístico e limitado por orçamento. O retorno inclui `estimated_tokens`, `dropped_count` e `budget_pressure`, permitindo que Agent/Assistant reajam à pressão de contexto em vez de descartar memória silenciosamente.

## 8. Consolidação conservadora

Conteúdo exatamente igual após normalização continua sendo deduplicado automaticamente. Similaridade não exata não é fundida de forma cega: `find_merge_candidates` retorna `Equivalent`, `NeedsReview` ou `Distinct`. Mudanças de tipo e possíveis mudanças de polaridade/negação são direcionadas para revisão.

## 9. Correções e conhecimento compartilhado

O trait `KnowledgeStore` oferece um ledger de correções com contador de reutilização, conceitos revisáveis com confiança/proveniência e links tipados ponderados. Essas estruturas são neutras ao produto: não conhecem turnos, sessões ou Dreaming do Agent.

`search_concept_neighborhood` parte de conceitos encontrados por FTS e expande relações de forma limitada e cycle-safe (até 4 hops e 100 conceitos). Isso permite recuperação orientada a grafo sem introduzir um banco de grafos residente.


## 10. Isolamento, concorrência e privacidade

Memórias `global` são canonicalizadas no escopo global; memórias `workspace` usam o workspace correspondente; memórias `conversation` exigem `scope_key`. Registros de conversa legados migram para a chave isolada `legacy`.

Writes críticos usam transações SQLite `IMMEDIATE`; a sensibilidade monotônica é resolvida atomicamente no SQL. Atualizações de telemetria de acesso não reconstruem o índice FTS. Correções e conceitos persistem sensibilidade e IDs de memórias de origem.
