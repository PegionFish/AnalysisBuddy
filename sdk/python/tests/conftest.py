"""共享测试工具：以内存流驱动 serve()，断言 stdout 帧序列。"""

import io
import json

import pytest


def req(method, params=None, rid=1):
    """构造宿主请求行（参数非法等场景由测试自行写原样 JSON）。"""
    msg = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        msg["params"] = params
    return json.dumps(msg, ensure_ascii=False)


def run_serve(plugin, requests, stderr=None):
    """以 BytesIO 驱动 serve()：请求全部预写 → EOF → serve 返回。

    返回 (stdout 行列表, stderr 文本)。
    """
    payload = "\n".join(requests) + "\n"
    stdin = io.BytesIO(payload.encode("utf-8"))
    stdout = io.BytesIO()
    err = io.StringIO() if stderr is None else stderr
    plugin.serve(stdin=stdin, stdout=stdout, stderr=err)
    frames = []
    for line in stdout.getvalue().decode("utf-8").splitlines():
        frames.append(json.loads(line))
    return frames, err.getvalue()


@pytest.fixture
def frames_helper():
    return run_serve
