local api = require("lib.ollama")

models = {}
model = ""
messages = {}
draft = ""
status = "Checking Ollama..."
problem = ""
ready = false
busy = false

local checking = false

local function has_model(name)
  for _, value in ipairs(models) do
    if value == name then
      return true
    end
  end

  return false
end

local function refresh()
  if checking then
    return
  end

  checking = true
  api.models(function(ok, names)
    checking = false

    if not ok then
      ready = false
      status = "Ollama is not running"
      problem = ""
      return
    end

    models = names
    ready = true

    if #models == 0 then
      model = ""
      status = "No models available"
    else
      if not has_model(model) then
        model = models[1]
      end
      status = ""
    end
  end)
end

function choose_model(value)
  if not ready or #models == 0 then
    return
  end

  model = value
end

function update_draft(value)
  draft = value
end

local function remove_empty(index)
  local message = messages[index]
  if message and message.role == "assistant" and message.content == "" then
    table.remove(messages, index)
  end
end

function send(text)
  if not ready or busy or #models == 0 then
    return
  end

  if type(text) == "string" then
    draft = text
  end

  local prompt = draft
  if prompt == "" then
    return
  end

  draft = ""
  problem = ""
  busy = true

  messages[#messages + 1] = { role = "user", content = prompt }

  local request = {}
  for index = 1, #messages do
    request[index] = messages[index]
  end

  messages[#messages + 1] = { role = "assistant", content = "" }
  local assistant = #messages

  local function line(raw)
    local decoded, chunk = pcall(nitroget.json_decode, raw)
    if not decoded or type(chunk) ~= "table" then
      return
    end

    if type(chunk.error) == "string" then
      problem = chunk.error
      return
    end

    local assistant_message = messages[assistant]
    if assistant_message
      and type(chunk.message) == "table"
      and type(chunk.message.content) == "string"
    then
      assistant_message.content = assistant_message.content .. chunk.message.content
    end
  end

  local function done(ok, message)
    busy = false

    if not ok then
      remove_empty(assistant)
      if message ~= "" then
        problem = message
      elseif problem == "" then
        problem = "request failed"
      end
      refresh()
      return
    end

    if message ~= "" and problem == "" then
      problem = message
    end

    remove_empty(assistant)
  end

  local started = api.chat(model, request, line, done)
  if not started then
    busy = false
    remove_empty(assistant)
    if problem == "" then
      problem = "request failed"
    end
  end
end

local function start()
  nitroget.exec("command -v curl >/dev/null 2>&1", function(ok)
    if not ok then
      status = "curl is required"
      return
    end

    refresh()
    nitroget.every(10000, function()
      if not busy then
        refresh()
      end
    end)
  end)
end

start()
