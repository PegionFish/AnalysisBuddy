import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-13-cap-bad-data"
    extra_capabilities = {"custom_query": True}

    def on_custom_query(self, req_id, params):
        if params.get("query") != "__probe__":
            # 未实现的查询名正确回 -32602，保持本 fixture 只制造 data 形状违规
            self.error(req_id, -32602, "invalid params")
            return
        # capabilities.custom_query = true 却对 probe 回成功且 data 非 JSON object
        # （数组；CustomQueryResult.data 必须为 object，可为空对象）
        # -> BEH-13 error（protocol-v1.md §2.11）
        self.reply(req_id, {"data": []})

if __name__ == "__main__":
    Plugin().run()
