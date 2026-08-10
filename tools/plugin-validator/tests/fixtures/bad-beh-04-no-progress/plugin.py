import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-04-no-progress"

    def on_parse(self, req_id, params):
        # 只发 RecordBatch，从不发 progress -> BEH-04
        self.send({"jsonrpc": "2.0", "method": "RecordBatch",
                   "params": {"file_id": params["file_id"], "seq": 0,
                              "records": [{"timestamp": 1, "metric": "fps", "value": 1.0}],
                              "done": True}})
        self.reply(req_id, {"records_total": 1})

if __name__ == "__main__":
    Plugin().run()
