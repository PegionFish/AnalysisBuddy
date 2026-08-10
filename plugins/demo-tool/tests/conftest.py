# pytest 夹具：把插件仓库根加入 sys.path（仓库根即插件目录，§4.6）；
# D1 路 SDK 未合入时注入开发期替身（analysisbuddy_stub）。真实 SDK 就绪后
# （pip install -e sdk/python），本 conftest 自动让位。

import importlib.util
import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

HAS_REAL_SDK = importlib.util.find_spec("analysisbuddy") is not None


@pytest.fixture(autouse=True)
def ensure_sdk_stub():
    if not HAS_REAL_SDK:
        import analysisbuddy_stub  # noqa: F401  注册 sys.modules["analysisbuddy"]
    yield


@pytest.fixture
def plugin():
    from main import DemoToolPlugin

    return DemoToolPlugin()
