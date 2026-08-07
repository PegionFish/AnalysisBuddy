import time

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-12-no-eof"

    def on_shutdown(self, req_id, params):
        self.reply(req_id, {})  # 应答但不退出

    def on_eof(self):
        time.sleep(3600)  # stdin EOF 后不自退 -> BEH-12（连带 BEH-10）

if __name__ == "__main__":
    Plugin().run()
