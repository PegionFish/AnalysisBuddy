import time

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-10-slow-exit"

    def on_shutdown(self, req_id, params):
        self.reply(req_id, {})
        time.sleep(4)  # 应答后 4s 才退出（> 3s x scale）-> BEH-10

if __name__ == "__main__":
    Plugin().run()
