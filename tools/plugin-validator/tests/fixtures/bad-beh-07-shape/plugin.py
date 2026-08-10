import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-07-shape"

    def on_key_values(self, req_id, params):
        self.reply(req_id, {"entries": "nope"})  # entries 非数组 -> BEH-07

if __name__ == "__main__":
    Plugin().run()
