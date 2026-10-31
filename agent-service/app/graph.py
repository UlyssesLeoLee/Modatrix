from __future__ import annotations

import json
import os
from typing import Any, TypedDict

import httpx
from langgraph.graph import END, START, StateGraph
from langgraph.types import interrupt


class WorkflowState(TypedDict, total=False):
    asset_id: str
    base_version: int
    requested_by: str
    feedback: str
    operations: list[dict[str, Any]]
    idempotency_key: str
    changeset_id: str
    preview: dict[str, Any]
    approved: bool
    approved_by: str
    commit_receipt: dict[str, Any]
    status: str
    planner_source: str


def _rust_headers() -> dict[str, str]:
    token = os.getenv("FF3D_API_TOKEN", "dev-insecure-token")
    return {"Authorization": f"Bearer {token}"}


async def _llm_plan(feedback: str) -> list[dict[str, Any]]:
    base_url = os.getenv("FF3D_LLM_BASE_URL", "").rstrip("/")
    model = os.getenv("FF3D_LLM_MODEL", "")
    if not base_url or not model:
        raise ValueError(
            "No structured operations supplied. Configure FF3D_LLM_BASE_URL and "
            "FF3D_LLM_MODEL for a local OpenAI-compatible model, or supply operations."
        )

    system_prompt = """
You are a constrained 3D editing planner. Convert the review feedback into JSON only:
{"operations":[...]}.
Allowed operation shapes:
1. {"kind":"set_vertex_position","mesh_id":"UUID","vertex_index":0,"position":[x,y,z]}
2. {"kind":"set_skin_weights","mesh_id":"UUID","vertex_index":0,"influences":[{"joint_index":0,"weight":1.0}]}
3. {"kind":"upsert_keyframe","clip_id":"UUID","joint_index":0,"channel":"rotation|translation|scale","time_ms":0,"value":[x,y,z,w]}
4. {"kind":"shift_keyframes","clip_id":"UUID","joint_index":0,"channel":"rotation|translation|scale","from_ms":0,"delta_ms":0}
Never invent UUIDs, mesh ids, clip ids, vertex indices, or joint indices. If the feedback lacks
required identifiers, return {"operations":[]} and explain nothing else. Do not emit code.
"""
    async with httpx.AsyncClient(timeout=90.0) as client:
        response = await client.post(
            f"{base_url}/chat/completions",
            json={
                "model": model,
                "temperature": 0,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": feedback},
                ],
                "response_format": {"type": "json_object"},
            },
        )
        response.raise_for_status()
        content = response.json()["choices"][0]["message"]["content"]
    payload = json.loads(content)
    operations = payload.get("operations", [])
    if not isinstance(operations, list):
        raise ValueError("local model returned an invalid operations array")
    return operations


async def plan_node(state: WorkflowState) -> dict[str, Any]:
    operations = state.get("operations") or []
    source = "caller"
    if not operations:
        operations = await _llm_plan(state.get("feedback", ""))
        source = "local_model"
    if not operations:
        raise ValueError("planner produced no operations; add semantic targets and try again")
    return {"operations": operations, "planner_source": source, "status": "planned"}


async def preview_node(state: WorkflowState) -> dict[str, Any]:
    base_url = os.getenv("FF3D_API_URL", "http://127.0.0.1:8080").rstrip("/")
    payload = {
        "base_version": state["base_version"],
        "requested_by": state.get("requested_by", "agent"),
        "idempotency_key": state["idempotency_key"],
        "operations": state["operations"],
    }
    async with httpx.AsyncClient(timeout=60.0) as client:
        response = await client.post(
            f"{base_url}/api/v1/assets/{state['asset_id']}/changesets/preview",
            headers=_rust_headers(),
            json=payload,
        )
        response.raise_for_status()
        preview = response.json()
    return {
        "changeset_id": preview["changeset_id"],
        "preview": preview,
        "status": "awaiting_approval",
    }


def approval_node(state: WorkflowState) -> dict[str, Any]:
    decision = interrupt(
        {
            "kind": "human_approval_required",
            "asset_id": state["asset_id"],
            "changeset_id": state["changeset_id"],
            "base_version": state["base_version"],
            "preview": state["preview"],
            "feedback": state.get("feedback", ""),
        }
    )
    return {
        "approved": bool(decision.get("approved", False)),
        "approved_by": decision.get("approved_by", "unknown"),
        "status": "approved" if decision.get("approved") else "rejected",
    }


def route_after_approval(state: WorkflowState) -> str:
    return "commit" if state.get("approved") else "rejected"


async def commit_node(state: WorkflowState) -> dict[str, Any]:
    base_url = os.getenv("FF3D_API_URL", "http://127.0.0.1:8080").rstrip("/")
    payload = {
        "approved_by": state.get("approved_by", "unknown"),
        "idempotency_key": state["idempotency_key"],
    }
    async with httpx.AsyncClient(timeout=60.0) as client:
        response = await client.post(
            f"{base_url}/api/v1/assets/{state['asset_id']}/changesets/"
            f"{state['changeset_id']}/commit",
            headers=_rust_headers(),
            json=payload,
        )
        response.raise_for_status()
        receipt = response.json()
    return {"commit_receipt": receipt, "status": "committed"}


def rejected_node(_: WorkflowState) -> dict[str, Any]:
    return {"status": "rejected"}


def build_graph(checkpointer: Any = None):
    builder = StateGraph(WorkflowState)
    builder.add_node("plan", plan_node)
    builder.add_node("preview", preview_node)
    builder.add_node("approval", approval_node)
    builder.add_node("commit", commit_node)
    builder.add_node("rejected", rejected_node)
    builder.add_edge(START, "plan")
    builder.add_edge("plan", "preview")
    builder.add_edge("preview", "approval")
    builder.add_conditional_edges(
        "approval",
        route_after_approval,
        {"commit": "commit", "rejected": "rejected"},
    )
    builder.add_edge("commit", END)
    builder.add_edge("rejected", END)
    return builder.compile(checkpointer=checkpointer)
