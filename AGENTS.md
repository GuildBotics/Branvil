# AGENTS.md

このファイルは、このリポジトリで作業する AI/自動化エージェント向けの作業ガイドです。
構造の説明は [`docs/architecture.md`](docs/architecture.md) に、文書の一覧は
[`README.md`](README.md) の「Documents」にあります。ここには、コードを読んでも分からず、
破ると困る約束事だけを書きます。

## 最重要ルール

- ソースコードを正とする（README / `docs/*.md` は参考情報）
- `.gitignore` 対象のファイル・ディレクトリは参照しない。特に `/data/`（開発用のデータディレクトリ。
  `secrets/owner.token` などの実トークンを含む）と `target/`
- 挙動変更を行った場合は、関連ドキュメントも必要に応じて更新する。HTTP API は
  [`docs/api.md`](docs/api.md) が契約で、GuildBotics の `branvil` provider がそれを前提にしている
- コードの修正に際しては「最小限の変更量」ではなく、「変更後のコード量が最小になること」を
  最優先事項とし、場当たり的対応ではなくあるべき姿の美しいコードとなることを心がける
- 後方互換性を維持するためのコード（移行フラグ、フォールバック読み込み、旧形式の並行サポート、
  deprecation 期間の新旧併存パス）を追加しない。互換性のために冗長なコードを残すより、
  常にあるべき姿のコードを維持することを優先する。破壊的な非互換が起こりうることは README の
  「Important Notice (Disclaimer)」で宣言済みであり、利用者は実質的にメンテナ本人のみである。
  - 保存形式を変えるときは `src/store.rs` の `SCHEMA_VERSION` を上げる。他の版のデータディレクトリと
    バックアップは変換せずに拒否する（変換処理を書かない）
  - HTTP API を変えるときは、互換の層を作らず、GuildBotics の `guildbotics/integrations/branvil/` を
    同時に直す
- 自然言語の意味判定（分類・採用可否など）は Branvil の責務ではない。必要になったら GuildBotics 側に置き、
  Branvil はキーワード列挙や文字列マッチで代用しない
- 責務境界を越えた実装をしない（下の「責務境界」）

## 責務境界

モジュールごとの責務は [`docs/architecture.md`](docs/architecture.md) の「Modules」が正本。
それとは別に、モジュールをまたいで守る境界は次のとおり。

- **Branvil は GuildBotics を知らない。** 依存は GuildBotics → Branvil の一方向で、Branvil の動作が
  GuildBotics を前提にするのは、`branvil runner` が探す msb の既定の場所
  （`src/runner/executor.rs` の `~/.guildbotics/data/msb`）だけ。GuildBotics を名指しするのは
  文書と、GuildBotics の契約テストを本物のサービスに流す仕組み（`integration/guildbotics/`、
  `scripts/guildbotics-contracts.sh`、`scripts/e2e-flow.sh`）に限る
- **upstream は provider に依らない境界の向こう。** `src/upstream/` はコードのホストを知らず、
  ホスト固有のことはすべて `Host`（`src/upstream/host.rs`）の provider ごとの実装
  （今は `src/upstream/github.rs` だけ）に置く
- **listen は loopback だけ。** 他のマシンからは `branvil tunnel` の SSH ポート転送で届く
  （`src/config.rs` が loopback 以外を拒否する）
- **データへの入口は HTTP API だけ。** GUI・GuildBotics の provider・`branvil` CLI のクライアントの
  コマンドはすべて API を使う（データディレクトリを直接扱うのは、サービスを止めて使う
  `branvil init`・`branvil admin check|reindex`・`branvil restore` だけ）
- **GUI は GuildBotics Desktop と同じ見た目にする。** Mantine の版、テーマ（`web/src/theme.ts`）、
  `web/src/styles.css` のデザイントークンとコンポーネントの上書きを GuildBotics Desktop と揃える
  （`docs/architecture.md` の「GUI files」）

## Git と資格情報

- サービスが動かす Git は `src/git.rs` を通す（サービスの `GIT_*` を引き継がず、system / global の
  設定を読まない）
- テスト・スクリプトでクライアントの Git を動かすときは、利用者の Git 設定と credential helper に
  触れない。`tests/common/mod.rs` の `git` / `git_as` を使い、同じ分離を自前で書くときも
  `GIT_CONFIG_NOSYSTEM=1`、空の `GIT_CONFIG_GLOBAL`、`GIT_TERMINAL_PROMPT=0`、
  `-c credential.helper=` を付け、トークンは `GIT_ASKPASS` と環境変数で渡す（URL や argv に入れない）。
  macOS では system の設定の `credential.helper=osxkeychain` がトークンを保存しようとして、
  利用者の画面にキーチェーンのダイアログが出る
- テストのメンバー名には実在の人の名前を使わない

## 確認コマンド（CI 準拠）

CI（[`.github/workflows/ci.yml`](.github/workflows/ci.yml)）と同じ確認:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 -m unittest discover -s tests -p test_dev_script.py -v
```

- 結合テストは 1 つのテストのクレート（`tests/main.rs`）にまとまっている。1 ファイルだけ流すときは
  `cargo test --test main <file>::`（例: `cargo test --test main tickets::`）
- 実機の microVM を使うテスト（`tests/real_executor.rs`）は `BRANVIL_REAL_EXECUTOR=1` のときだけ動く。
  通常の確認には含めない
