import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-13-no-cap-wrong-code"

    def on_custom_query(self, req_id, params):
        # capabilities.custom_query 未声明（缺省 false）却被调 custom_query 时回
        # 集合外错误码 -32603（只允许 -32005 / -32601，宿主归一 unsupported）
        # -> BEH-13 error（protocol-v1.md §2.11/§4.2）
        self.error(req_id, -32603, "internal error")

if __name__ == "__main__":
    Plugin().run()
