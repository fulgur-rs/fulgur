# frozen_string_literal: true

module Fulgur
  class Margin
    class << self
      alias_method :__native_new__, :new

      def new(*args, **kwargs)
        unless kwargs.empty?
          raise ArgumentError, "positional and kwargs are mutually exclusive" unless args.empty?

          required = %i[top right bottom left]
          missing = required - kwargs.keys
          raise ArgumentError, "missing keys: #{missing.join(", ")}" unless missing.empty?

          t, r, b, l = kwargs.values_at(*required).map(&:to_f)
          return __build__(t, r, b, l)
        end

        case args.length
        when 1
          v = args[0].to_f
          __build__(v, v, v, v)
        when 2
          vv, hh = args.map(&:to_f)
          __build__(vv, hh, vv, hh)
        when 4
          t, r, b, l = args.map(&:to_f)
          __build__(t, r, b, l)
        else
          raise ArgumentError, "wrong number of arguments (#{args.length} for 1, 2, 4, or kwargs)"
        end
      end
    end
  end
end
