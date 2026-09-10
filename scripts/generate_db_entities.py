#!/usr/bin/env python3
"""
Generate kabipay-db-entities Rust sources from Liquibase tenant migrations.
Run from repo root:  python hrms-svc/scripts/generate_db_entities.py
"""
from __future__ import annotations

import argparse
import re
import xml.etree.ElementTree as ET
from pathlib import Path

NS = {"db": "http://www.liquibase.org/xml/ns/dbchangelog"}
REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MIGRATIONS = REPOSITORY_ROOT / "hrms-database/changelog/migrations"
TENANT_CHANGELOG_MASTER = (
    REPOSITORY_ROOT / "hrms-database/changelog/tenant.changelog-master.xml"
)
OUT = REPOSITORY_ROOT / "hrms-svc/crates/kabipay-db-entities/src/tenant"

COMPOSITE_PK: dict[str, list[str]] = {
    "role_permission": ["role_id", "permission_id"],
    "user_role": ["user_id", "role_id"],
}


def sql_type_to_rust(sql_type: str, nullable: bool) -> str:
    st = sql_type.strip()
    if st == "UUID":
        base = "Uuid"
    elif st == "BOOLEAN":
        base = "bool"
    elif st == "INT":
        base = "i32"
    elif st == "BIGINT":
        base = "i64"
    elif re.match(r"^NUMERIC\(\d+,\d+\)$", st):
        base = "Decimal"
    elif st == "TIMESTAMPTZ":
        base = "DateTimeUtc"
    elif st == "DATE":
        base = "NaiveDate"
    elif st == "TIME":
        base = "NaiveTime"
    elif st == "JSONB":
        base = "Json"
    elif st.startswith("VARCHAR"):
        base = "String"
    elif st == "TEXT":
        base = "String"
    else:
        raise ValueError(f"Unknown SQL type: {sql_type}")
    if nullable:
        return f"Option<{base}>"
    return base


def col_nullable(col: ET.Element) -> bool:
    cons = col.find("db:constraints", NS)
    if cons is None:
        return True
    return cons.get("nullable", "true") != "false"


def parse_columns(table_el: ET.Element) -> list[tuple[str, str, bool]]:
    cols = []
    for col in table_el.findall("db:column", NS):
        name = col.attrib["name"]
        ctype = col.attrib["type"]
        nullable = col_nullable(col)
        cols.append((name, ctype, nullable))
    return cols


def sanitize_mod_name(table: str) -> str:
    if table in ("mod", "type", "use", "self", "crate"):
        return f"{table}_"
    return table.replace("-", "_")


def rust_field_name(col: str) -> str:
    """Escape Rust keywords used as SQL column names."""
    if col == "type":
        return "r#type"
    return col


def emit_entity(
    table: str,
    cols: list[tuple[str, str, bool]],
    primary_key_columns: list[str] | None = None,
) -> str:
    mod = sanitize_mod_name(table)
    composite = primary_key_columns or COMPOSITE_PK.get(table)
    lines = [
        f"pub mod {mod} {{",
        "    use crate::tenant::prelude::*;",
        "",
        "    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]",
        f'    #[sea_orm(table_name = "{table}")]',
        "    pub struct Model {",
    ]
    for name, ctype, nullable in cols:
        if composite and name in composite:
            rs_t = sql_type_to_rust(ctype, nullable)
            lines.append(
                "        #[sea_orm(primary_key, auto_increment = false)]"
            )
            lines.append(f"        pub {rust_field_name(name)}: {rs_t},")
            continue
        if composite:
            rs_t = sql_type_to_rust(ctype, nullable)
            lines.append(f"        pub {rust_field_name(name)}: {rs_t},")
            continue
        if name == "id" and ctype == "UUID":
            lines.append(
                "        #[sea_orm(primary_key, auto_increment = false)]"
            )
            lines.append("        pub id: Uuid,")
            continue
        rs_t = sql_type_to_rust(ctype, nullable)
        lines.append(f"        pub {rust_field_name(name)}: {rs_t},")
    lines.append("    }")
    lines.append("")
    lines.append("    impl ActiveModelBehavior for ActiveModel {}")
    lines.append("")
    lines.append("    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]")
    lines.append("    pub enum Relation {}")
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def forward_changes(path: Path) -> list[ET.Element]:
    """Return changeset children in file order without rollback descendants."""
    tree = ET.parse(path)
    root = tree.getroot()
    return [
        change
        for changeset in root.findall(".//db:changeSet", NS)
        for change in list(changeset)
        if change.tag != f"{{{NS['db']}}}rollback"
    ]


def process_file(path: Path) -> list[str]:
    chunks: list[str] = []
    for ct in forward_changes(path):
        if ct.tag != f"{{{NS['db']}}}createTable":
            continue
        schema = ct.attrib.get("schemaName", "")
        if schema != "${schema}":
            continue
        table = ct.attrib["tableName"]
        cols = parse_columns(ct)
        if not cols:
            continue
        chunks.append(emit_entity(table, cols))
    return chunks


def tenant_migration_files() -> list[Path]:
    """Return tenant migration XML files in authoritative master-include order."""
    tree = ET.parse(TENANT_CHANGELOG_MASTER)
    master_directory = TENANT_CHANGELOG_MASTER.parent
    migrations_root = MIGRATIONS.resolve()
    files: list[Path] = []
    for include in tree.getroot().findall("db:include", NS):
        include_path = include.attrib.get("file")
        if not include_path:
            continue
        path = (master_directory / include_path).resolve()
        try:
            path.relative_to(migrations_root)
        except ValueError as error:
            raise ValueError(
                f"Tenant migration include is outside {MIGRATIONS}: {include_path}"
            ) from error
        files.append(path)
    return files


def migration_directories() -> list[Path]:
    """Return included migration directories in first-include order."""
    return list(dict.fromkeys(path.parent for path in tenant_migration_files()))


def collect_domain_tables(
    directory: Path,
) -> tuple[
    dict[str, list[tuple[str, str, bool]]],
    dict[str, list[str]],
    list[str],
]:
    """Collect one domain's tables after all later forward amendments."""
    selected = directory.resolve()
    tables: dict[str, list[tuple[str, str, bool]]] = {}
    primary_keys: dict[str, list[str]] = {}
    sources: list[str] = []
    selected_reached = False

    for xml in tenant_migration_files():
        migration_directory = xml.parent
        if migration_directory.resolve() == selected:
            selected_reached = True
        if not selected_reached:
            continue

        source_used = False
        for change in forward_changes(xml):
            schema = change.attrib.get("schemaName", "")
            table = change.attrib.get("tableName")
            if schema != "${schema}" or table is None:
                continue

            if (
                migration_directory.resolve() == selected
                and change.tag == f"{{{NS['db']}}}createTable"
            ):
                columns = parse_columns(change)
                if columns:
                    tables[table] = columns
                    source_used = True
                continue

            if table not in tables:
                continue

            if change.tag == f"{{{NS['db']}}}addPrimaryKey":
                primary_keys[table] = [
                    column.strip()
                    for column in change.attrib.get("columnNames", "").split(",")
                    if column.strip()
                ]
                source_used = True
            elif change.tag == f"{{{NS['db']}}}addColumn":
                existing_names = {name for name, _, _ in tables[table]}
                for column in parse_columns(change):
                    if column[0] not in existing_names:
                        tables[table].append(column)
                        existing_names.add(column[0])
                source_used = True
            elif change.tag == f"{{{NS['db']}}}dropColumn":
                dropped_names = {
                    column.attrib["name"]
                    for column in change.findall("db:column", NS)
                }
                if column_name := change.attrib.get("columnName"):
                    dropped_names.add(column_name)
                tables[table] = [
                    column for column in tables[table] if column[0] not in dropped_names
                ]
                source_used = True

        if source_used:
            sources.append(xml.relative_to(REPOSITORY_ROOT).as_posix())

    return tables, primary_keys, sources


def domain_rust_mod(folder_name: str) -> str:
    """0005_auth_rbac -> d0005_auth_rbac (valid Rust identifier)."""
    return f"d{folder_name}"


def generate_domain(directory: Path) -> str:
    """Generate one migration directory and return its Rust module name."""
    if "integration_connector" in directory.name and "0005_integration" in directory.name:
        return ""

    tables, primary_keys, sources = collect_domain_tables(directory)
    if not tables:
        return ""

    module_name = domain_rust_mod(directory.name)
    src_comment = ", ".join(sources)
    body = "\n".join(
        emit_entity(table, columns, primary_keys.get(table))
        for table, columns in tables.items()
    )
    (OUT / f"{module_name}.rs").write_text(
        f"//! Auto-generated from `{src_comment}`.\n\n{body}",
        encoding="utf-8",
    )
    return module_name


def merge_module_export(module_name: str) -> None:
    """Add one generated migration module without rewriting other module exports."""
    if not module_name:
        return

    mod_path = OUT / "mod.rs"
    contents_bytes = mod_path.read_bytes()
    newline = "\r\n" if b"\r\n" in contents_bytes else "\n"
    contents = contents_bytes.decode("utf-8")
    lines = contents.splitlines()
    export = f"pub mod {module_name};"
    if export in lines:
        return

    domain_export = re.compile(r"pub mod d\d{4}_[A-Za-z0-9_]+;")
    first_domain_export = next(
        (index for index, line in enumerate(lines) if domain_export.fullmatch(line)),
        None,
    )
    if first_domain_export is None:
        lines.extend(["", export])
    else:
        last_domain_export = first_domain_export
        while (
            last_domain_export + 1 < len(lines)
            and domain_export.fullmatch(lines[last_domain_export + 1])
        ):
            last_domain_export += 1
        domain_exports = lines[first_domain_export : last_domain_export + 1]
        lines[first_domain_export : last_domain_export + 1] = sorted(
            [*domain_exports, export]
        )

    mod_path.write_bytes((newline.join(lines) + newline).encode("utf-8"))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--only",
        help="Generate one migration directory without rewriting other modules",
    )
    args = parser.parse_args()

    OUT.mkdir(parents=True, exist_ok=True)
    if args.only:
        selected = MIGRATIONS / args.only
        if not selected.is_dir() or not re.match(r"^\d{4}_", selected.name):
            raise SystemExit(f"Unknown tenant migration directory: {args.only}")
        merge_module_export(generate_domain(selected))
        return

    dirs = migration_directories()
    mod_lines: list[str] = []

    for directory in dirs:
        module_name = generate_domain(directory)
        if module_name:
            mod_lines.append(f"pub mod {module_name};")

    prelude = """//! Shared imports for generated tenant entities.
pub use sea_orm::entity::prelude::*;
pub use sea_orm::prelude::Json;
pub use sea_orm::{
    ActiveModelBehavior, DeriveEntityModel, DeriveRelation, EnumIter, RelationTrait,
};
pub use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
pub use rust_decimal::Decimal;
pub use uuid::Uuid;

pub type DateTimeUtc = DateTime<Utc>;
"""
    (OUT / "prelude.rs").write_text(prelude, encoding="utf-8")

    # mod.rs
    unique_mods = sorted(set(mod_lines))
    mod_rs = (
        "//! Tenant-schema table models (Liquibase domains 0005–0030).\n"
        "//! Generated — do not hand-edit; re-run `scripts/generate_db_entities.py`.\n\n"
        "pub mod prelude;\n"
        "pub use prelude::*;\n\n"
        + "\n".join(unique_mods)
        + "\n"
    )
    (OUT / "mod.rs").write_text(mod_rs, encoding="utf-8")

    print(f"Wrote {len(unique_mods)} modules under {OUT}")


if __name__ == "__main__":
    main()
