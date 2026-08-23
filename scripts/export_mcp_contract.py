#!/usr/bin/env python3
"""Export the Python server's MCP tool contract for Rust parity checks."""

from __future__ import annotations

import argparse
import asyncio
from dataclasses import asdict, is_dataclass
from datetime import date, datetime, timedelta
from enum import Enum
import importlib
from importlib import metadata
import json
import os
from pathlib import Path
import sys
from typing import Any

MODES = ("legacy", "oauth21", "gateway")
_GATEWAY_EXPORT_ENV = {
    "GATEWAY_IDENTITY_JWKS_URL": "https://contract-export.invalid/.well-known/jwks.json",
    "GATEWAY_IDENTITY_AUDIENCE": "workspace-mcp-contract-export",
}


def _configure_mode(mode: str) -> None:
    reset_names = (
        "MCP_ENABLE_OAUTH21",
        "EXTERNAL_OAUTH21_PROVIDER",
        "TRUST_GATEWAY_IDENTITY",
        *_GATEWAY_EXPORT_ENV,
    )
    for name in reset_names:
        os.environ.pop(name, None)
    if mode == "oauth21":
        os.environ["MCP_ENABLE_OAUTH21"] = "true"
    elif mode == "gateway":
        os.environ["TRUST_GATEWAY_IDENTITY"] = "true"
        os.environ.update(_GATEWAY_EXPORT_ENV)


def _jsonable(value: Any) -> Any:
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, Enum):
        return _jsonable(value.value)
    if isinstance(value, timedelta):
        return {"seconds": value.total_seconds()}
    if isinstance(value, (date, datetime)):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): _jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(item) for item in value]
    if isinstance(value, (set, frozenset)):
        return sorted(_jsonable(item) for item in value)
    if hasattr(value, "model_dump"):
        return _jsonable(value.model_dump(mode="python", exclude_none=True))
    if is_dataclass(value):
        return _jsonable(asdict(value))
    raise TypeError(f"Unsupported contract value: {type(value).__name__}")


def _tool_contract(tool: Any) -> dict[str, Any]:
    return {
        "name": tool.name,
        "version": tool.version,
        "title": tool.title,
        "description": tool.description,
        "icons": _jsonable(tool.icons),
        "tags": _jsonable(tool.tags),
        "meta": _jsonable(tool.meta),
        "parameters": _jsonable(tool.parameters),
        "output_schema": _jsonable(tool.output_schema),
        "annotations": _jsonable(tool.annotations),
        "task_config": _jsonable(tool.task_config),
    }


async def _export(mode: str) -> dict[str, Any]:
    _configure_mode(mode)

    from main import SERVICE_MODULES

    for module_name in SERVICE_MODULES.values():
        importlib.import_module(module_name)

    from core.server import server
    from core.tool_registry import filter_server_tools, set_enabled_tools
    from core.tool_tier_loader import ToolTierLoader

    complete_tools = set(ToolTierLoader().get_tools_up_to_tier("complete"))
    set_enabled_tools(complete_tools)
    filter_server_tools(server)
    tools = await server.list_tools(run_middleware=False)

    return {
        "format_version": 1,
        "source": "python-fastmcp",
        "workspace_mcp_version": metadata.version("workspace-mcp"),
        "mode": mode,
        "tier": "complete",
        "tool_count": len(tools),
        "tools": [_tool_contract(tool) for tool in tools],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=MODES, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    contract = asyncio.run(_export(args.mode))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(
        json.dumps(contract, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    temporary.replace(args.output)
    sys.__stdout__.write(
        f"exported {contract['tool_count']} tools for {args.mode} to {args.output}\n"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
