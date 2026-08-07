import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "abstain-plugin"

    def on_can_handle(self, req_id, params):
        self.reply(req_id, {"can_handle": False, "confidence": 0.0,
                            "reason": "abstain fixture"})

if __name__ == "__main__":
    Plugin().run()
