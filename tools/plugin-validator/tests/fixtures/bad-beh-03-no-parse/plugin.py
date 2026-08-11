import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-03-no-parse"

    def on_parse(self, req_id, params):
        # 模拟 SDK 缺省占位（sdk/analysisbuddy/plugin.py on_parse 未覆写时回 -32005，
        # E-08 产品决策）：parse 是必选方法，回 unsupported_in_v1 = 未实现 -> BEH-03
        self.error(req_id, -32005, "parse not implemented by this plugin")

if __name__ == "__main__":
    Plugin().run()
