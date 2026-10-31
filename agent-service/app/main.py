from __future__ import annotations

import os
from contextlib import asynccontextmanager
from typing import Any
from uuid import uuid4

from fastapi import FastAPI, Header, HTTPException, Request
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.types import Command
from pydantic import BaseModel, Field

from app.graph import build_graph


class StartRunRequest(BaseModel):
    asset_id: str
    base_version: int = Field(ge=1)
    requested_by: str = "local-user"
    feedback: str
    operations: list[dict[str, Any]] = Field(default_factory=list)
    thread_id: str | None = None
    idempotency_key: str | None = None


class ResumeRunRequest(BaseModel):
    approved: bool
    approved_by: str = "local-user"


def _agent_auth(expected: str, supplied: str | None) -> None:
    if expected and supplied != f"Bearer {expected}":
        raise HTTPException(status_code=401, detail="invalid agent service token")


@asynccontextmanager
async def lifespan(app: FastAPI):
    checkpointer_cm = None
    database_url = os.getenv("FF3D_LANGGRAPH_DATABASE_URL")
    if database_url:
        from langgraph.checkpoint.postgres import PostgresSaver

        checkpointer_cm = PostgresSaver.from_conn_string(database_url)
        checkpointer = checkpointer_cm.__enter__()
        checkpointer.setup()
    else:
        checkpointer = InMemorySaver()

    app.state.graph = build_graph(checkpointer)
    yield

    if checkpointer_cm is not None:
        checkpointer_cm.__exit__(None, None, None)


app = FastAPI(
    title="FrameForge 3D Agent Service",
    version="0.1.0",
    description="Official LangGraph orchestration; Rust remains the authoritative asset editor.",
    lifespan=lifespan,
)


@app.get("/health")
async def health():
    return {
        "service": "frameforge3d-agent",
        "status": "ok",
        "checkpoint": "postgres" if os.getenv("FF3D_LANGGRAPH_DATABASE_URL") else "memory",
    }


@app.post("/v1/runs")
async def start_run(
    body: StartRunRequest,
    request: Request,
    authorization: str | None = Header(default=None),
):
    _agent_auth(os.getenv("FF3D_AGENT_TOKEN", ""), authorization)
    thread_id = body.thread_id or str(uuid4())
    idempotency_key = body.idempotency_key or str(uuid4())
    initial_state = {
        "asset_id": body.asset_id,
        "base_version": body.base_version,
        "requested_by": body.requested_by,
        "feedback": body.feedback,
        "operations": body.operations,
        "idempotency_key": idempotency_key,
        "status": "started",
    }
    config = {"configurable": {"thread_id": thread_id}}
    try:
        result = await request.app.state.graph.ainvoke(initial_state, config=config)
    except Exception as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc

    interrupted = result.get("__interrupt__")
    if interrupted:
        return {
            "thread_id": thread_id,
            "status": "awaiting_approval",
            "interrupt": [item.value for item in interrupted],
        }
    return {"thread_id": thread_id, **result}


@app.post("/v1/runs/{thread_id}/resume")
async def resume_run(
    thread_id: str,
    body: ResumeRunRequest,
    request: Request,
    authorization: str | None = Header(default=None),
):
    _agent_auth(os.getenv("FF3D_AGENT_TOKEN", ""), authorization)
    config = {"configurable": {"thread_id": thread_id}}
    try:
        result = await request.app.state.graph.ainvoke(
            Command(resume={"approved": body.approved, "approved_by": body.approved_by}),
            config=config,
        )
    except Exception as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    return {"thread_id": thread_id, **result}
