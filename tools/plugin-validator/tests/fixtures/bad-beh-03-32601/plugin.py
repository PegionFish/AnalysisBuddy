import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-03-32601"

    def on_schema(self, req_id, params):
        self.error(req_id, -32601, "Method not found")  # 必选方法回 -32601 -> BEH-03

if __name__ == "__main__":
    Plugin().run()
