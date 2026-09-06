import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "good-beh-13-query"
    extra_capabilities = {"custom_query": True}

    def on_custom_query(self, req_id, params):
        # 合规实现（protocol-v1.md §2.11）：probe 回成功且 data 为 JSON object
        # （可为空对象）；未实现的查询名回 -32602 invalid params
        if params.get("query") == "__probe__":
            self.reply(req_id, {"data": {"rows": 0}})
        else:
            self.error(req_id, -32602, "invalid params")

if __name__ == "__main__":
    Plugin().run()
