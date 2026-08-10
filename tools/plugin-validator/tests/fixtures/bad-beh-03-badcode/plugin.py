import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-03-badcode"

    def on_initialize(self, req_id, params):
        self.error(req_id, -9999, "custom code out of set")  # 非标准错误码 -> BEH-03

if __name__ == "__main__":
    Plugin().run()
