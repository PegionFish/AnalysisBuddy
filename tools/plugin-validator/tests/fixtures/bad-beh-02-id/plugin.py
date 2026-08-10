import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-02-id"

    def reply(self, req_id, result):
        self.send({"jsonrpc": "2.0", "id": req_id + 1, "result": result})  # 响应 id 错位 -> BEH-02

if __name__ == "__main__":
    Plugin().run()
