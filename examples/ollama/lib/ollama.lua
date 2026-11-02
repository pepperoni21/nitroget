local base = "http://127.0.0.1:11434"

local function models(callback)
  nitroget.exec("curl -fsS --max-time 2 " .. base .. "/api/tags", function(ok, output)
    if not ok then
      callback(false, {}, output)
      return
    end

    local decoded, data = pcall(nitroget.json_decode, output)
    if not decoded or type(data) ~= "table" then
      callback(false, {}, "invalid response")
      return
    end

    local list = data.models
    if type(list) ~= "table" then
      callback(false, {}, "invalid response")
      return
    end

    local names = {}
    for _, item in ipairs(list) do
      if type(item) == "table" then
        local name = item.name
        if type(name) == "string" and name ~= "" then
          names[#names + 1] = name
        end
      end
    end

    table.sort(names)
    callback(true, names, "")
  end)
end

local function chat(model, messages, on_line, on_done)
  local encoded, body = pcall(nitroget.json_encode, {
    model = model,
    messages = messages,
    stream = true,
  })

  if not encoded then
    on_done(false, tostring(body))
    return nil
  end

  local command = "curl -sS -N --max-time 300 "
    .. "-H 'Content-Type: application/json' --data-binary @- "
    .. base .. "/api/chat"

  local started, result = pcall(nitroget.stream, command, body, on_line, on_done)
  if not started then
    on_done(false, tostring(result))
    return nil
  end

  return result
end

return {
  models = models,
  chat = chat,
}
