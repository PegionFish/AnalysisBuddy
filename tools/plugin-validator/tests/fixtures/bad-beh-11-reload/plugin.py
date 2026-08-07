import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-11-reload"

    def on_load_file(self, req_id, params):
        self._load_count += 1
        if self._load_count > 1:
            self.error(req_id, -32002, "file load failed")  # 二次 load 失败 -> BEH-11
            return
        self.loaded[params["file_id"]] = params["path"]
        self.reply(req_id, {})

if __name__ == "__main__":
    Plugin().run()
