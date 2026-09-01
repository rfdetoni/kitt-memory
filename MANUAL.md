# Manual do K.I.T.T. Memory (`kitt-memory`)

> Motor de armazenamento de memória persistente local com SQLite WAL, sensibilidade monotônica, isolamento de segurança e migração de schema canônico.

---

## 1. Visão Geral e Arquitetura

O **`kitt-memory`** provê o armazenamento de longa duração (long-term memory) para o assistente e o agente de codificação.
Ele gerencia memórias episódicas, decisões arquiteturais, preferências de usuário e contexto histórico de projetos.

### Componentes:
- **`kitt-memory-core`**: Definição de domínio, níveis de sensibilidade monotônica (`Public`, `Internal`, `Confidential`, `Secret`) e normalização.
- **`kitt-memory-sqlite`**: Driver de alta concorrência SQLite em modo WAL com transações atômicas e permissões de arquivo privadas (`0600` em Unix).
- **`kitt-memory-migrate`**: Ferramenta CLI para inspecionar, reparar e migrar bases de dados de versões legadas para o **Schema Canônico V1**.

---

## 2. Requisitos de Sistema

- **Rust**: 1.80+ (com `cargo`)
- **SQLite**: 3.35+ (compilado estaticamente ou dinâmico via `libsqlite3`)

---

## 3. Instalação e Compilação por Sistema Operacional

### 🐧 A. LINUX (Ubuntu/Debian/Fedora)

```bash
# Instalar dependências SQLite (se necessário)
sudo apt-get install -y sqlite3 libsqlite3-dev

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

### Variáveis de Ambiente:
```bash
# Sobrescrever caminho do banco de dados
export KITT_MEMORY_DB_PATH="/caminho/personalizado/memory.db"

# Habilitar logs detalhados
export RUST_LOG="kitt_memory=debug"
```

---

## 5. Guia de Uso da CLI `kitt-memory-migrate`

### Inspecionar Status e Schema do Banco:
```bash
cargo run --release --bin kitt-memory-migrate -- --db ~/.kitt/history/history.sqlite3 --status
```

### Migrar Base Legada (ex: v4 ou agent-cli legada) para Schema V1:
```bash
cargo run --release --bin kitt-memory-migrate -- --source /caminho/antigo.db --target ~/.kitt/history/history.sqlite3 --migrate
```

### Executar Verificação de Integridade e Re-indexação:
```bash
cargo run --release --bin kitt-memory-migrate -- --db ~/.kitt/history/history.sqlite3 --vacuum
```

---

## 6. Políticas de Segurança e Privacidade
1. **Permissões de Arquivo**: No Unix/macOS, o arquivo de banco é sempre criado com modo `0600` (leitura/escrita apenas pelo usuário).
2. **Rejeição de Symlinks**: Symlinks são estritamente rejeitados para evitar ataques de redirecionamento de caminho.
3. **Sensibilidade Monotônica**: Uma vez gravada como confidencial ou secreta, uma memória nunca sofre downgrade de sensibilidade por atualizações parciais.
